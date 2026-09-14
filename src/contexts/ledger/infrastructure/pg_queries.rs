//! Pool-backed read-only Ledger query adapter.

use std::collections::HashMap;

use async_trait::async_trait;
use rust_decimal::Decimal;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::infrastructure::database::VerifiedDatabase;
use crate::shared_kernel::{CorrelationId, CurrencyCode, Money, UserId};

use super::super::{
    application::ports::LedgerQueryPort,
    domain::{
        AccountAuthority, AccountKind, AccountNature, Actor, AnnotationVersion, AssignmentOrigin,
        AutomationState, BalanceVersion, JournalEntryId, JournalRelations, JournalSource,
        LedgerAccountId, LedgerError, ObservationId, PostingId, PostingPurpose,
        ReconciliationCaseId, ReconciliationStatus, ReconciliationVersion, SourceReference,
    },
    public::{
        AccountView, ActivityCursor, ActivityFilter, ActivitySummary, ActivityTotal,
        CorrectionView, JournalAnnotationView, JournalView, PostingView, ReconciliationView,
    },
};
use super::rows::AccountRow;
use crate::contexts::ledger::public::{
    AnalyticsCalendar, AnalyticsCalendarRequest, AnalyticsFact, AnalyticsFilter, AnalyticsInterval,
    AnalyticsPage, AnalyticsTransactionsQuery,
};

/// SELECT-only accounting-fact queries.
#[derive(Clone)]
pub(crate) struct PgLedgerQueries {
    pub(super) pool: PgPool,
}

#[derive(FromRow)]
struct JournalRow {
    transfer_conversion_id: Option<Uuid>,
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
    annotation_description: Option<String>,
    category_id: Option<Uuid>,
    assignment_origin: Option<String>,
    classification_decision_id: Option<Uuid>,
    automation_state: Option<String>,
    annotation_note: Option<String>,
    annotation_tags: Option<Vec<String>>,
    annotation_budget_visibility: Option<String>,
    annotation_created_at: Option<chrono::DateTime<chrono::Utc>>,
    annotation_updated_at: Option<chrono::DateTime<chrono::Utc>>,
    reversed_by_journal_id: Option<Uuid>,
    replaced_by_journal_id: Option<Uuid>,
    correction_account_id: Option<Uuid>,
    correction_before: Option<Decimal>,
    correction_target: Option<Decimal>,
    correction_delta: Option<Decimal>,
    correction_balance_version: Option<i64>,
    correction_reason: Option<String>,
    correction_observed_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(FromRow)]
struct PostingRow {
    journal_entry_id: Uuid,
    id: Uuid,
    account_id: Uuid,
    account_kind: String,
    account_authority: String,
    position: i16,
    currency: String,
    account_nature: String,
    signed_amount: Decimal,
}

#[derive(FromRow)]
struct ActivitySummaryRow {
    transaction_count: i64,
    category_count: i64,
    currency: Option<String>,
    amount: Option<Decimal>,
}

impl PgLedgerQueries {
    pub(crate) fn new(pool: &VerifiedDatabase) -> Self {
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
        ).bind(user_id.into_uuid()).fetch_all(&self.pool).await.map_err(LedgerError::storage)?;
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
         .fetch_optional(&self.pool).await.map_err(LedgerError::storage)?
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
        .map_err(LedgerError::storage)?;
        self.load_journals(user_id, &ids).await
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
        .map_err(LedgerError::storage)?;
        self.load_journals(user_id, &ids).await
    }

    pub(crate) async fn list_activity(
        &self,
        user_id: UserId,
        filter: ActivityFilter,
        after: Option<ActivityCursor>,
        limit: u32,
    ) -> Result<Vec<JournalView>, LedgerError> {
        if limit == 0 || limit > 200 {
            return Err(LedgerError::invalid_state(
                "activity limit must be 1 to 200",
            ));
        }
        let category_ids = filter
            .category_ids()
            .iter()
            .map(|id| id.into_uuid())
            .collect::<Vec<_>>();
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT j.id FROM ledger.journal_entries j \
             WHERE j.user_id = $1 \
               AND (NOT $12 OR (j.purpose <> 'reversal' AND j.reverses_transaction_id IS NULL \
                   AND NOT EXISTS (SELECT 1 FROM ledger.journal_entries reversed \
                       WHERE reversed.user_id = j.user_id AND reversed.reverses_transaction_id = j.id))) \
               AND (NOT $10 OR NOT EXISTS(SELECT 1 FROM ledger.transfer_conversion_journals cj JOIN ledger.transfer_conversions cv ON cv.user_id=cj.user_id AND cv.id=cj.conversion_id WHERE cj.user_id=j.user_id AND cj.journal_id=j.id AND (cj.role IN ('source','reversal') OR (cj.role='transfer' AND NOT cv.active)))) AND ($11::uuid IS NULL OR EXISTS(SELECT 1 FROM ledger.postings ap WHERE ap.user_id=j.user_id AND ap.journal_entry_id=j.id AND ap.account_id=$11)) AND j.occurred_at >= $2 AND j.occurred_at < $3 \
               AND ($4 = 'all' OR EXISTS ( \
                   SELECT 1 FROM ledger.postings flow \
                   WHERE flow.user_id = j.user_id AND flow.journal_entry_id = j.id \
                     AND flow.account_nature IN ('income', 'expense') \
                   GROUP BY flow.currency \
                   HAVING ($4 = 'income' AND -SUM(flow.signed_amount) > 0) \
                       OR ($4 = 'expense' AND -SUM(flow.signed_amount) < 0) \
               )) \
               AND (cardinality($5::uuid[]) = 0 OR EXISTS ( \
                   SELECT 1 FROM ledger.transaction_annotations selected \
                   WHERE selected.user_id=j.user_id AND selected.journal_entry_id=j.id \
                     AND selected.category_id=ANY($5) \
               )) \
               AND (NOT $6 OR NOT EXISTS ( \
                   SELECT 1 FROM ledger.transaction_annotations categorized \
                   WHERE categorized.user_id=j.user_id AND categorized.journal_entry_id=j.id \
                     AND categorized.category_id IS NOT NULL \
               )) \
               AND ($7::timestamptz IS NULL OR (j.occurred_at, j.ledger_sequence) < ($7, $8)) \
             ORDER BY j.occurred_at DESC, j.ledger_sequence DESC LIMIT $9",
        )
        .bind(user_id.into_uuid())
        .bind(filter.from_occurred_at())
        .bind(filter.before_occurred_at())
        .bind(filter.kind().as_str())
        .bind(&category_ids)
        .bind(filter.uncategorized())
        .bind(after.map(|cursor| cursor.occurred_at))
        .bind(after.map(|cursor| cursor.ledger_sequence))
        .bind(i64::from(limit))
        .bind(filter.grouped_transfers())
        .bind(filter.account_id().map(|i| i.into_uuid()))
        .bind(filter.hide_reversed())
        .fetch_all(&self.pool)
        .await
        .map_err(LedgerError::storage)?;
        self.load_journals(user_id, &ids).await
    }

    pub(crate) async fn summarize_activity(
        &self,
        user_id: UserId,
        filter: ActivityFilter,
    ) -> Result<ActivitySummary, LedgerError> {
        let category_ids = filter
            .category_ids()
            .iter()
            .map(|id| id.into_uuid())
            .collect::<Vec<_>>();
        let rows = sqlx::query_as::<_, ActivitySummaryRow>(
            "WITH matching_journals AS ( \
                 SELECT j.id FROM ledger.journal_entries j \
                 WHERE j.user_id = $1 \
                   AND (NOT $9 OR (j.purpose <> 'reversal' AND j.reverses_transaction_id IS NULL \
                       AND NOT EXISTS (SELECT 1 FROM ledger.journal_entries reversed \
                           WHERE reversed.user_id = j.user_id AND reversed.reverses_transaction_id = j.id))) \
                   AND (NOT $7 OR NOT EXISTS(SELECT 1 FROM ledger.transfer_conversion_journals cj JOIN ledger.transfer_conversions cv ON cv.user_id=cj.user_id AND cv.id=cj.conversion_id WHERE cj.user_id=j.user_id AND cj.journal_id=j.id AND (cj.role IN ('source','reversal') OR (cj.role='transfer' AND NOT cv.active)))) AND ($8::uuid IS NULL OR EXISTS(SELECT 1 FROM ledger.postings ap WHERE ap.user_id=j.user_id AND ap.journal_entry_id=j.id AND ap.account_id=$8)) AND j.occurred_at >= $2 AND j.occurred_at < $3 \
                   AND ($4 = 'all' OR EXISTS ( \
                       SELECT 1 FROM ledger.postings flow \
                       WHERE flow.user_id = j.user_id AND flow.journal_entry_id = j.id \
                         AND flow.account_nature IN ('income', 'expense') \
                       GROUP BY flow.currency \
                       HAVING ($4 = 'income' AND -SUM(flow.signed_amount) > 0) \
                           OR ($4 = 'expense' AND -SUM(flow.signed_amount) < 0) \
                   )) \
                   AND (cardinality($5::uuid[]) = 0 OR EXISTS ( \
                       SELECT 1 FROM ledger.transaction_annotations selected \
                       WHERE selected.user_id=j.user_id AND selected.journal_entry_id=j.id \
                         AND selected.category_id=ANY($5) \
                   )) \
                   AND (NOT $6 OR NOT EXISTS ( \
                       SELECT 1 FROM ledger.transaction_annotations categorized \
                       WHERE categorized.user_id=j.user_id AND categorized.journal_entry_id=j.id \
                         AND categorized.category_id IS NOT NULL \
                   )) \
             ), counts AS ( \
                 SELECT COUNT(*)::bigint AS transaction_count, \
                        COUNT(DISTINCT a.category_id)::bigint AS category_count \
                 FROM matching_journals m \
                 LEFT JOIN ledger.transaction_annotations a \
                   ON a.user_id = $1 AND a.journal_entry_id = m.id \
             ), totals AS ( \
                 SELECT p.currency, -SUM(p.signed_amount) AS amount \
                 FROM matching_journals m \
                 JOIN ledger.postings p ON p.user_id = $1 AND p.journal_entry_id = m.id \
                 WHERE p.account_nature IN ('income', 'expense') \
                 GROUP BY p.currency HAVING SUM(p.signed_amount) <> 0 \
             ) \
             SELECT c.transaction_count, c.category_count, t.currency, t.amount \
             FROM counts c LEFT JOIN totals t ON TRUE \
             ORDER BY ABS(t.amount) DESC NULLS LAST, t.currency",
        )
        .bind(user_id.into_uuid())
        .bind(filter.from_occurred_at())
        .bind(filter.before_occurred_at())
        .bind(filter.kind().as_str())
        .bind(&category_ids)
        .bind(filter.uncategorized())
        .bind(filter.grouped_transfers())
        .bind(filter.account_id().map(|i| i.into_uuid()))
        .bind(filter.hide_reversed())
        .fetch_all(&self.pool)
        .await
        .map_err(LedgerError::storage)?;

        let first = rows
            .first()
            .ok_or_else(|| LedgerError::persistence("activity summary returned no counts"))?;
        let totals = rows
            .iter()
            .filter_map(|row| row.currency.as_ref().zip(row.amount))
            .map(|(currency, amount)| {
                Ok(ActivityTotal {
                    currency: CurrencyCode::new(currency.clone())
                        .map_err(|_| LedgerError::persistence("stored currency invalid"))?,
                    amount,
                })
            })
            .collect::<Result<Vec<_>, LedgerError>>()?;
        Ok(ActivitySummary {
            transaction_count: first.transaction_count,
            category_count: first.category_count,
            totals,
        })
    }

    pub(crate) async fn get_journal(
        &self,
        user_id: UserId,
        id: JournalEntryId,
    ) -> Result<JournalView, LedgerError> {
        self.load_journals(user_id, &[id.into_uuid()])
            .await?
            .into_iter()
            .next()
            .ok_or_else(LedgerError::not_found)
    }

    async fn load_journals(
        &self,
        user_id: UserId,
        ids: &[Uuid],
    ) -> Result<Vec<JournalView>, LedgerError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        let rows = sqlx::query_as::<_, JournalRow>(
            "SELECT (SELECT cj.conversion_id FROM ledger.transfer_conversion_journals cj WHERE cj.user_id=j.user_id AND cj.journal_id=j.id AND cj.role<>'restoration' LIMIT 1) AS transfer_conversion_id, j.id, j.user_id, j.ledger_sequence, j.source, j.purpose, COALESCE((SELECT cv.document->>'title' FROM ledger.transfer_conversions cv JOIN ledger.transfer_conversion_journals cj ON cj.user_id=cv.user_id AND cj.conversion_id=cv.id WHERE cj.user_id=j.user_id AND cj.journal_id=j.id AND cj.role='transfer'),j.description) AS description, j.actor_kind, j.actor_reference, j.occurred_at, \
                    j.recorded_at, j.correlation_id, j.reverses_transaction_id, \
                    j.corrects_transaction_id, j.replaces_transaction_id, a.version AS annotation_version, \
                    a.description AS annotation_description, a.category_id, a.assignment_origin, \
                    a.classification_decision_id, a.automation_state, a.note AS annotation_note, \
                    a.tags AS annotation_tags, a.budget_visibility AS annotation_budget_visibility, \
                    a.created_at AS annotation_created_at, a.updated_at AS annotation_updated_at, \
                    reversed.id AS reversed_by_journal_id, replacement.id AS replaced_by_journal_id, \
                    c.account_id AS correction_account_id, c.before_display_balance AS correction_before, \
                    c.target_display_balance AS correction_target, c.display_delta AS correction_delta, \
                    c.observed_balance_version AS correction_balance_version, c.reason AS correction_reason, \
                    c.observed_at AS correction_observed_at \
             FROM ledger.journal_entries j LEFT JOIN ledger.transaction_annotations a \
               ON a.journal_entry_id = j.id AND a.user_id = j.user_id \
             LEFT JOIN ledger.journal_entries reversed \
               ON reversed.reverses_transaction_id = j.id AND reversed.user_id = j.user_id \
             LEFT JOIN ledger.journal_entries replacement \
               ON replacement.replaces_transaction_id = j.id AND replacement.user_id = j.user_id \
             LEFT JOIN ledger.balance_correction_details c ON c.journal_entry_id = j.id AND c.user_id = j.user_id \
             WHERE j.user_id = $1 AND j.id = ANY($2)",
        )
        .bind(user_id.into_uuid())
        .bind(ids)
        .fetch_all(&self.pool)
        .await
        .map_err(LedgerError::storage)?;

        let posting_rows = sqlx::query_as::<_, PostingRow>(
            "SELECT p.journal_entry_id, p.id, p.account_id, p.position, p.currency, p.account_nature, p.signed_amount, \
                    a.kind AS account_kind,a.authority AS account_authority \
             FROM ledger.postings p JOIN ledger.accounts a ON a.id=p.account_id AND a.user_id=p.user_id \
             WHERE p.user_id = $1 AND p.journal_entry_id = ANY($2) \
             ORDER BY p.journal_entry_id, p.position",
        )
        .bind(user_id.into_uuid())
        .bind(ids)
        .fetch_all(&self.pool)
        .await
        .map_err(LedgerError::storage)?;

        let mut postings_by_journal = HashMap::<Uuid, Vec<PostingView>>::with_capacity(ids.len());
        for posting in posting_rows {
            postings_by_journal
                .entry(posting.journal_entry_id)
                .or_default()
                .push(posting.into_view()?);
        }

        let mut rows_by_id = rows
            .into_iter()
            .map(|row| (row.id, row))
            .collect::<HashMap<_, _>>();
        let mut views = Vec::with_capacity(ids.len());
        for id in ids {
            let row = rows_by_id.remove(id).ok_or_else(LedgerError::not_found)?;
            views.push(row.into_view(postings_by_journal.remove(id).unwrap_or_default())?);
        }
        Ok(views)
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
        ).bind(user_id.into_uuid()).fetch_all(&self.pool).await.map_err(LedgerError::storage)?;
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
        .map_err(LedgerError::storage)?
        .ok_or_else(LedgerError::not_found)?
        .into_view()
    }
}

impl PostingRow {
    fn into_view(self) -> Result<PostingView, LedgerError> {
        let nature = AccountNature::parse(&self.account_nature)?;
        Ok(PostingView {
            id: PostingId::new(self.id),
            account_id: LedgerAccountId::new(self.account_id),
            account_kind: AccountKind::parse(&self.account_kind)?,
            account_nature: nature,
            account_authority: AccountAuthority::parse(&self.account_authority)?,
            position: u16::try_from(self.position)
                .map_err(|_| LedgerError::persistence("stored position invalid"))?,
            currency: CurrencyCode::new(self.currency)
                .map_err(|_| LedgerError::persistence("stored currency invalid"))?,
            signed_amount: self.signed_amount,
            display_effect: self.signed_amount * Decimal::from(nature.normal_sign()),
        })
    }
}

impl JournalRow {
    fn into_view(self, postings: Vec<PostingView>) -> Result<JournalView, LedgerError> {
        let relations = if let Some(related) = self.reverses_transaction_id {
            JournalRelations::reversal_of(JournalEntryId::new(related))
        } else if let Some(related) = self.corrects_transaction_id {
            JournalRelations::correction_of(JournalEntryId::new(related))
        } else if let Some(related) = self.replaces_transaction_id {
            JournalRelations::replacement_of(JournalEntryId::new(related))
        } else {
            JournalRelations::none()
        };
        Ok(JournalView {
            transfer_conversion_id: self.transfer_conversion_id,
            id: JournalEntryId::new(self.id),
            user_id: UserId::new(self.user_id),
            ledger_sequence: self.ledger_sequence,
            source: match self.source.as_str() {
                "manual" => JournalSource::Manual,
                "import" => JournalSource::Import,
                "system" => JournalSource::System,
                "correction" => JournalSource::Correction,
                "reconciliation" => JournalSource::Reconciliation,
                _ => return Err(LedgerError::persistence("stored source invalid")),
            },
            purpose: PostingPurpose::parse(&self.purpose)?,
            actor: match self.actor_kind.as_str() {
                "user" => Actor::User(UserId::new(
                    Uuid::parse_str(self.actor_reference.as_deref().unwrap_or(""))
                        .map_err(|_| LedgerError::persistence("stored user actor is invalid"))?,
                )),
                "system" => Actor::System,
                "external" => {
                    let reference = self.actor_reference.unwrap_or_default();
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
            description: self.description,
            occurred_at: self.occurred_at,
            recorded_at: self.recorded_at,
            correlation_id: CorrelationId::new(self.correlation_id),
            relations,
            postings,
            annotation: match (
                self.annotation_version,
                self.annotation_description,
                self.annotation_tags,
                self.annotation_budget_visibility,
                self.annotation_created_at,
                self.annotation_updated_at,
            ) {
                (
                    Some(version),
                    Some(description),
                    Some(tags),
                    Some(budget_visibility),
                    Some(created_at),
                    Some(updated_at),
                ) => Some(JournalAnnotationView {
                    version: AnnotationVersion::new(version)?,
                    description,
                    category_id: self
                        .category_id
                        .map(crate::contexts::classification::public::CategoryId::new),
                    assignment_origin: self
                        .assignment_origin
                        .as_deref()
                        .map(AssignmentOrigin::parse)
                        .transpose()?,
                    classification_decision_id: self.classification_decision_id,
                    automation_state: AutomationState::parse(
                        self.automation_state.as_deref().ok_or_else(|| {
                            LedgerError::persistence(
                                "stored transaction annotation automation state is missing",
                            )
                        })?,
                    )?,
                    note: self.annotation_note,
                    tags,
                    budget_visibility: match budget_visibility.as_str() {
                        "included" => super::super::domain::BudgetVisibility::Included,
                        "excluded" => super::super::domain::BudgetVisibility::Excluded,
                        _ => {
                            return Err(LedgerError::persistence(
                                "stored budget visibility is invalid",
                            ));
                        }
                    },
                    created_at,
                    updated_at,
                }),
                (None, None, None, None, None, None) => None,
                _ => {
                    return Err(LedgerError::persistence(
                        "stored transaction annotation is incomplete",
                    ));
                }
            },
            reversed_by_journal_id: self.reversed_by_journal_id.map(JournalEntryId::new),
            replaced_by_journal_id: self.replaced_by_journal_id.map(JournalEntryId::new),
            correction: match (
                self.correction_account_id,
                self.correction_before,
                self.correction_target,
                self.correction_delta,
                self.correction_balance_version,
                self.correction_reason,
                self.correction_observed_at,
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
}

#[async_trait]
impl LedgerQueryPort for PgLedgerQueries {
    async fn analytics_calendar(
        &self,
        request: AnalyticsCalendarRequest,
    ) -> Result<AnalyticsCalendar, LedgerError> {
        super::analytics::analytics_calendar(&self.pool, request).await
    }

    async fn analytics_aggregate(
        &self,
        user_id: UserId,
        filter: AnalyticsFilter,
        intervals: Vec<AnalyticsInterval>,
    ) -> Result<Vec<AnalyticsFact>, LedgerError> {
        super::analytics::analytics_aggregate(&self.pool, user_id, filter, intervals).await
    }

    async fn analytics_transactions(
        &self,
        user_id: UserId,
        query: AnalyticsTransactionsQuery,
    ) -> Result<AnalyticsPage, LedgerError> {
        super::analytics::analytics_transactions(&self.pool, user_id, query).await
    }

    async fn list_accounts(&self, user_id: UserId) -> Result<Vec<AccountView>, LedgerError> {
        PgLedgerQueries::list_accounts(self, user_id).await
    }

    async fn get_account(
        &self,
        user_id: UserId,
        id: LedgerAccountId,
    ) -> Result<AccountView, LedgerError> {
        PgLedgerQueries::get_account(self, user_id, id).await
    }

    async fn account_activity(
        &self,
        user_id: UserId,
        account_id: LedgerAccountId,
        after: Option<ActivityCursor>,
        limit: u32,
    ) -> Result<Vec<JournalView>, LedgerError> {
        PgLedgerQueries::account_activity(self, user_id, account_id, after, limit).await
    }

    async fn list_journals(
        &self,
        user_id: UserId,
        after: Option<ActivityCursor>,
        limit: u32,
    ) -> Result<Vec<JournalView>, LedgerError> {
        PgLedgerQueries::list_journals(self, user_id, after, limit).await
    }

    async fn list_activity(
        &self,
        user_id: UserId,
        filter: ActivityFilter,
        after: Option<ActivityCursor>,
        limit: u32,
    ) -> Result<Vec<JournalView>, LedgerError> {
        PgLedgerQueries::list_activity(self, user_id, filter, after, limit).await
    }

    async fn summarize_activity(
        &self,
        user_id: UserId,
        filter: ActivityFilter,
    ) -> Result<ActivitySummary, LedgerError> {
        PgLedgerQueries::summarize_activity(self, user_id, filter).await
    }

    async fn get_journal(
        &self,
        user_id: UserId,
        id: JournalEntryId,
    ) -> Result<JournalView, LedgerError> {
        PgLedgerQueries::get_journal(self, user_id, id).await
    }

    async fn list_reconciliations(
        &self,
        user_id: UserId,
    ) -> Result<Vec<ReconciliationView>, LedgerError> {
        PgLedgerQueries::list_reconciliations(self, user_id).await
    }

    async fn get_reconciliation(
        &self,
        user_id: UserId,
        id: ReconciliationCaseId,
    ) -> Result<ReconciliationView, LedgerError> {
        PgLedgerQueries::get_reconciliation(self, user_id, id).await
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
