//! Transaction-bound PostgreSQL aggregate stores.

use rust_decimal::Decimal;
use sqlx::FromRow;
use uuid::Uuid;

use crate::integration::{IntegrationEvent, outbox::OutboxWriter, postgres::PgOutboxWriter};
use crate::shared_kernel::{CurrencyCode, IdempotencyKey, Money, UserId};

use super::{pg_unit_of_work::PgLedgerTransaction, rows::AccountRow};
use super::super::{
    application::ports::{
        AnnotationStore, AuditRecord, AuditStore, CommandReceiptStore, CorrectionDetail,
        CorrectionStore, JournalSnapshot, JournalStore, LedgerAccountStore, LedgerOutboxStore,
        ProjectionStore, ReconciliationStore, ReconciliationStream, StoredReceipt,
    },
    domain::{
        AccountNature, Actor, AnnotationId, AnnotationVersion, BudgetVisibility, CategoryReference,
        JournalEntry, JournalEntryId, LedgerAccount, LedgerAccountId, LedgerError, NormalizedTags,
        BalanceObservation, BalanceVersion, ObservationId, Posting, PostingId, ReconciliationCase,
        ReconciliationCaseId, ReconciliationStatus, ReconciliationVersion, SourceReference,
        SystemAccountRole, TransactionAnnotation,
    },
};

const ACCOUNT_COLUMNS: &str =
    "id, user_id, name, currency, nature, kind, authority, visibility, lifecycle, \
     system_role, version, created_at, updated_at";

#[derive(FromRow)]
struct ReconciliationRow {
    id: Uuid, user_id: Uuid, account_id: Uuid, observation_id: Uuid,
    source_kind: String, source_stream_id: String, source_item_id: String,
    observed_at: chrono::DateTime<chrono::Utc>, source_sequence: i64,
    recorded_at: chrono::DateTime<chrono::Utc>, provider_reported_balance: Decimal,
    available_balance: Option<Decimal>, currency: String, captured_ledger_balance: Decimal,
    captured_balance_version: i64, delta: Decimal, status: String, version: i64,
    approval_journal_id: Option<Uuid>, reason: Option<String>,
    decision_actor_kind: Option<String>, decision_actor_reference: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>, updated_at: chrono::DateTime<chrono::Utc>,
}

impl ReconciliationRow {
    fn into_domain(self) -> Result<ReconciliationCase, LedgerError> {
        let currency = CurrencyCode::new(self.currency)
            .map_err(|_| LedgerError::persistence("stored reconciliation currency is invalid"))?;
        let source = SourceReference::new(self.source_kind, self.source_stream_id, self.source_item_id)?;
        let observation = BalanceObservation::new(
            ObservationId::new(self.observation_id), source,
            Money::new(self.provider_reported_balance, currency.clone(), 8)
                .map_err(|error| LedgerError::persistence(error.to_string()))?,
            self.available_balance.map(|amount| Money::new(amount, currency.clone(), 8))
                .transpose().map_err(|error| LedgerError::persistence(error.to_string()))?,
            self.observed_at, self.source_sequence, self.recorded_at,
        )?;
        let actor = match self.decision_actor_kind.as_deref() {
            Some("user") => self.decision_actor_reference.as_deref()
                .and_then(|value| Uuid::parse_str(value).ok()).map(UserId::new).map(Actor::User),
            Some("system") => Some(Actor::System),
            Some("external") => Some(Actor::External {
                source_kind: observation.source().source_kind().to_owned(),
                source_reference: self.decision_actor_reference.unwrap_or_default(),
            }),
            _ => None,
        };
        ReconciliationCase::rehydrate(
            ReconciliationCaseId::new(self.id), UserId::new(self.user_id),
            LedgerAccountId::new(self.account_id), observation,
            Money::new(self.captured_ledger_balance, currency.clone(), 8)
                .map_err(|error| LedgerError::persistence(error.to_string()))?,
            BalanceVersion::new(self.captured_balance_version)?,
            Money::new(self.delta, currency, 8).map_err(|error| LedgerError::persistence(error.to_string()))?,
            ReconciliationStatus::parse(&self.status)?, ReconciliationVersion::new(self.version)?,
            self.approval_journal_id.map(JournalEntryId::new), actor, self.reason,
            self.created_at, self.updated_at,
        )
    }
}

impl LedgerAccountStore for PgLedgerTransaction<'_> {
    async fn find_account(
        &mut self,
        user_id: UserId,
        id: LedgerAccountId,
        lock: bool,
    ) -> Result<Option<LedgerAccount>, LedgerError> {
        let sql = if lock {
            "SELECT id, user_id, name, currency, nature, kind, authority, visibility, lifecycle, \
                    system_role, version, created_at, updated_at \
             FROM ledger.accounts WHERE id = $1 AND user_id = $2 FOR UPDATE"
        } else {
            "SELECT id, user_id, name, currency, nature, kind, authority, visibility, lifecycle, \
                    system_role, version, created_at, updated_at \
             FROM ledger.accounts WHERE id = $1 AND user_id = $2"
        };
        sqlx::query_as::<_, AccountRow>(sql)
            .bind(id.into_uuid())
            .bind(user_id.into_uuid())
            .fetch_optional(&mut *self.transaction)
            .await
            .map_err(LedgerError::database)?
            .map(AccountRow::into_domain)
            .transpose()
    }

    async fn lock_accounts(
        &mut self,
        user_id: UserId,
        ids: &[LedgerAccountId],
    ) -> Result<Vec<LedgerAccount>, LedgerError> {
        let mut ids: Vec<Uuid> = ids.iter().map(|id| id.into_uuid()).collect();
        ids.sort_unstable();
        ids.dedup();
        let rows = sqlx::query_as::<_, AccountRow>(
            "SELECT id, user_id, name, currency, nature, kind, authority, visibility, lifecycle, \
                    system_role, version, created_at, updated_at \
             FROM ledger.accounts \
             WHERE user_id = $1 AND id = ANY($2) \
             ORDER BY id FOR UPDATE",
        )
        .bind(user_id.into_uuid())
        .bind(&ids)
        .fetch_all(&mut *self.transaction)
        .await
        .map_err(LedgerError::database)?;
        rows.into_iter().map(AccountRow::into_domain).collect()
    }

    async fn insert_account(&mut self, account: &LedgerAccount) -> Result<(), LedgerError> {
        sqlx::query(
            "INSERT INTO ledger.accounts \
             (id, user_id, name, currency, nature, kind, authority, visibility, lifecycle, \
              system_role, version, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        )
        .bind(account.id().into_uuid())
        .bind(account.user_id().into_uuid())
        .bind(account.name())
        .bind(account.currency().as_str())
        .bind(account.nature().as_str())
        .bind(account.kind().as_str())
        .bind(account.authority().as_str())
        .bind(account.visibility().as_str())
        .bind(account.lifecycle().as_str())
        .bind(account.system_role().map(SystemAccountRole::as_str))
        .bind(account.version().get())
        .bind(account.created_at())
        .bind(account.updated_at())
        .execute(&mut *self.transaction)
        .await
        .map_err(LedgerError::database)?;

        sqlx::query(
            "INSERT INTO ledger.account_balances \
             (account_id, user_id, currency, signed_balance, version, as_of) \
             VALUES ($1, $2, $3, 0, 1, $4)",
        )
        .bind(account.id().into_uuid())
        .bind(account.user_id().into_uuid())
        .bind(account.currency().as_str())
        .bind(account.created_at())
        .execute(&mut *self.transaction)
        .await
        .map_err(LedgerError::database)?;
        Ok(())
    }

    async fn insert_system_account(&mut self, account: &LedgerAccount, subject_reference: &str) -> Result<(), LedgerError> {
        sqlx::query(
            "INSERT INTO ledger.accounts \
             (id, user_id, name, currency, nature, kind, authority, visibility, lifecycle, \
              system_role, system_subject_reference, version, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)",
        ).bind(account.id().into_uuid()).bind(account.user_id().into_uuid()).bind(account.name())
         .bind(account.currency().as_str()).bind(account.nature().as_str()).bind(account.kind().as_str())
         .bind(account.authority().as_str()).bind(account.visibility().as_str()).bind(account.lifecycle().as_str())
         .bind(account.system_role().map(SystemAccountRole::as_str)).bind(subject_reference)
         .bind(account.version().get()).bind(account.created_at()).bind(account.updated_at())
         .execute(&mut *self.transaction).await.map_err(LedgerError::database)?;
        sqlx::query("INSERT INTO ledger.account_balances (account_id,user_id,currency,signed_balance,version,as_of) VALUES ($1,$2,$3,0,1,$4)")
            .bind(account.id().into_uuid()).bind(account.user_id().into_uuid())
            .bind(account.currency().as_str()).bind(account.created_at())
            .execute(&mut *self.transaction).await.map_err(LedgerError::database)?;
        Ok(())
    }

    async fn save_account(&mut self, account: &LedgerAccount) -> Result<(), LedgerError> {
        let previous_version = account
            .version()
            .get()
            .checked_sub(1)
            .ok_or_else(LedgerError::version_conflict)?;
        let result = sqlx::query(
            "UPDATE ledger.accounts \
             SET name = $3, lifecycle = $4, version = $5, updated_at = $6 \
             WHERE id = $1 AND user_id = $2 AND version = $7",
        )
        .bind(account.id().into_uuid())
        .bind(account.user_id().into_uuid())
        .bind(account.name())
        .bind(account.lifecycle().as_str())
        .bind(account.version().get())
        .bind(account.updated_at())
        .bind(previous_version)
        .execute(&mut *self.transaction)
        .await
        .map_err(LedgerError::database)?;
        if result.rows_affected() != 1 {
            return Err(LedgerError::version_conflict());
        }
        Ok(())
    }

    async fn find_system_account(
        &mut self,
        user_id: UserId,
        currency: &CurrencyCode,
        role: SystemAccountRole,
        subject_reference: Option<&str>,
    ) -> Result<Option<LedgerAccount>, LedgerError> {
        let lock_identity = format!(
            "{}:{}:{}:{}",
            user_id,
            currency,
            role.as_str(),
            subject_reference.unwrap_or("")
        );
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(lock_identity)
            .execute(&mut *self.transaction)
            .await
            .map_err(LedgerError::database)?;
        let row = sqlx::query_as::<_, AccountRow>(&format!(
            "SELECT {ACCOUNT_COLUMNS} FROM ledger.accounts \
             WHERE user_id = $1 AND currency = $2 AND system_role = $3 \
               AND system_subject_reference IS NOT DISTINCT FROM $4 \
             FOR UPDATE"
        ))
        .bind(user_id.into_uuid())
        .bind(currency.as_str())
        .bind(role.as_str())
        .bind(subject_reference)
        .fetch_optional(&mut *self.transaction)
        .await
        .map_err(LedgerError::database)?;
        row.map(AccountRow::into_domain).transpose()
    }
}

impl JournalStore for PgLedgerTransaction<'_> {
    async fn find_journal(
        &mut self,
        user_id: UserId,
        id: JournalEntryId,
        lock: bool,
    ) -> Result<Option<JournalSnapshot>, LedgerError> {
        let sql = if lock {
            "SELECT description FROM ledger.journal_entries WHERE id = $1 AND user_id = $2 FOR UPDATE"
        } else {
            "SELECT description FROM ledger.journal_entries WHERE id = $1 AND user_id = $2"
        };
        let Some(description): Option<String> = sqlx::query_scalar(sql)
            .bind(id.into_uuid()).bind(user_id.into_uuid())
            .fetch_optional(&mut *self.transaction).await.map_err(LedgerError::database)?
        else { return Ok(None) };
        #[derive(FromRow)]
        struct PostingRow {
            id: Uuid, position: i16, account_id: Uuid, user_id: Uuid,
            currency: String, account_nature: String, signed_amount: Decimal,
        }
        let rows = sqlx::query_as::<_, PostingRow>(
            "SELECT id, position, account_id, user_id, currency, account_nature, signed_amount \
             FROM ledger.postings WHERE journal_entry_id = $1 AND user_id = $2 ORDER BY position",
        ).bind(id.into_uuid()).bind(user_id.into_uuid())
          .fetch_all(&mut *self.transaction).await.map_err(LedgerError::database)?;
        let postings = rows.into_iter().map(|row| {
            Ok(Posting::rehydrate(
                PostingId::new(row.id), u16::try_from(row.position)
                    .map_err(|_| LedgerError::persistence("stored posting position is invalid"))?,
                LedgerAccountId::new(row.account_id), UserId::new(row.user_id),
                CurrencyCode::new(row.currency)
                    .map_err(|_| LedgerError::persistence("stored posting currency is invalid"))?,
                AccountNature::parse(&row.account_nature)?, row.signed_amount,
            ))
        }).collect::<Result<Vec<_>, LedgerError>>()?;
        Ok(Some(JournalSnapshot { id, user_id, description, postings }))
    }

    async fn insert_journal(
        &mut self,
        command_name: &str,
        journal: &JournalEntry,
    ) -> Result<i64, LedgerError> {
        let (actor_kind, actor_reference) = actor_columns(journal.actor());
        let sequence: i64 = sqlx::query_scalar(
            "INSERT INTO ledger.journal_entries \
             (id, user_id, command_name, source, purpose, description, actor_kind, actor_reference, \
              occurred_at, recorded_at, correlation_id, causation_id, idempotency_key, \
              reverses_transaction_id, corrects_transaction_id, replaces_transaction_id, fx_rate) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17) \
             RETURNING ledger_sequence",
        )
        .bind(journal.id().into_uuid())
        .bind(journal.user_id().into_uuid())
        .bind(command_name)
        .bind(journal.source().as_str())
        .bind(journal.purpose().as_str())
        .bind(journal.description())
        .bind(actor_kind)
        .bind(actor_reference)
        .bind(journal.occurred_at())
        .bind(journal.recorded_at())
        .bind(journal.correlation_id().into_uuid())
        .bind(journal.causation_id().map(|id| id.into_uuid()))
        .bind(journal.idempotency_key().as_str())
        .bind(journal.relations().reverses().map(|id| id.into_uuid()))
        .bind(journal.relations().corrects().map(|id| id.into_uuid()))
        .bind(journal.relations().replaces().map(|id| id.into_uuid()))
        .bind(journal.fx_rate())
        .fetch_one(&mut *self.transaction)
        .await
        .map_err(LedgerError::database)?;

        for posting in journal.postings() {
            sqlx::query(
                "INSERT INTO ledger.postings \
                 (id, journal_entry_id, user_id, account_id, currency, account_nature, position, signed_amount) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(posting.id().into_uuid())
            .bind(journal.id().into_uuid())
            .bind(posting.user_id().into_uuid())
            .bind(posting.account_id().into_uuid())
            .bind(posting.currency().as_str())
            .bind(posting.account_nature().as_str())
            .bind(i16::try_from(posting.position()).map_err(|_| LedgerError::too_few_postings())?)
            .bind(posting.signed_amount())
            .execute(&mut *self.transaction)
            .await
            .map_err(LedgerError::database)?;
        }
        Ok(sequence)
    }
}

impl AnnotationStore for PgLedgerTransaction<'_> {
    async fn find_annotation(
        &mut self,
        user_id: UserId,
        journal_entry_id: JournalEntryId,
        lock: bool,
    ) -> Result<Option<TransactionAnnotation>, LedgerError> {
        #[derive(FromRow)]
        struct Row {
            id: Uuid, journal_entry_id: Uuid, user_id: Uuid, description: String,
            category_id: Option<Uuid>, note: Option<String>, tags: Vec<String>,
            budget_visibility: String, version: i64,
            created_at: chrono::DateTime<chrono::Utc>, updated_at: chrono::DateTime<chrono::Utc>,
        }
        let sql = if lock {
            "SELECT id, journal_entry_id, user_id, description, category_id, note, tags, \
                    budget_visibility, version, created_at, updated_at \
             FROM ledger.transaction_annotations \
             WHERE journal_entry_id = $1 AND user_id = $2 FOR UPDATE"
        } else {
            "SELECT id, journal_entry_id, user_id, description, category_id, note, tags, \
                    budget_visibility, version, created_at, updated_at \
             FROM ledger.transaction_annotations WHERE journal_entry_id = $1 AND user_id = $2"
        };
        let row = sqlx::query_as::<_, Row>(sql)
            .bind(journal_entry_id.into_uuid()).bind(user_id.into_uuid())
            .fetch_optional(&mut *self.transaction).await.map_err(LedgerError::database)?;
        row.map(|row| TransactionAnnotation::rehydrate(
            AnnotationId::new(row.id), JournalEntryId::new(row.journal_entry_id), UserId::new(row.user_id),
            row.description, row.category_id.map(CategoryReference::new), row.note,
            NormalizedTags::new(row.tags)?,
            match row.budget_visibility.as_str() {
                "included" => BudgetVisibility::Included,
                "excluded" => BudgetVisibility::Excluded,
                _ => return Err(LedgerError::persistence("stored budget visibility is invalid")),
            },
            AnnotationVersion::new(row.version)?, row.created_at, row.updated_at,
        )).transpose()
    }

    async fn insert_annotation(
        &mut self,
        annotation: &TransactionAnnotation,
    ) -> Result<(), LedgerError> {
        let tags: Vec<&str> = annotation.tags().as_slice().iter().map(String::as_str).collect();
        sqlx::query(
            "INSERT INTO ledger.transaction_annotations \
             (id, journal_entry_id, user_id, description, category_id, note, tags, \
              budget_visibility, version, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(annotation.id().into_uuid())
        .bind(annotation.journal_entry_id().into_uuid())
        .bind(annotation.user_id().into_uuid())
        .bind(annotation.description())
        .bind(annotation.category().map(|id| id.into_uuid()))
        .bind(annotation.note())
        .bind(&tags)
        .bind(annotation.budget_visibility().as_str())
        .bind(annotation.version().get())
        .bind(annotation.created_at())
        .bind(annotation.updated_at())
        .execute(&mut *self.transaction)
        .await
        .map_err(LedgerError::database)?;
        Ok(())
    }

    async fn save_annotation(&mut self, annotation: &TransactionAnnotation) -> Result<(), LedgerError> {
        let tags: Vec<&str> = annotation.tags().as_slice().iter().map(String::as_str).collect();
        let result = sqlx::query(
            "UPDATE ledger.transaction_annotations SET description = $3, category_id = $4, note = $5, \
             tags = $6, budget_visibility = $7, version = $8, updated_at = $9 \
             WHERE journal_entry_id = $1 AND user_id = $2 AND version = $10",
        ).bind(annotation.journal_entry_id().into_uuid()).bind(annotation.user_id().into_uuid())
         .bind(annotation.description()).bind(annotation.category().map(|id| id.into_uuid()))
         .bind(annotation.note()).bind(&tags).bind(annotation.budget_visibility().as_str())
         .bind(annotation.version().get()).bind(annotation.updated_at())
         .bind(annotation.version().get() - 1)
         .execute(&mut *self.transaction).await.map_err(LedgerError::database)?;
        if result.rows_affected() != 1 { return Err(LedgerError::version_conflict()) }
        Ok(())
    }
}

impl CorrectionStore for PgLedgerTransaction<'_> {
    async fn insert_correction_detail(&mut self, detail: CorrectionDetail<'_>) -> Result<(), LedgerError> {
        sqlx::query(
            "INSERT INTO ledger.balance_correction_details \
             (journal_entry_id, user_id, account_id, currency, before_display_balance, \
              target_display_balance, display_delta, observed_balance_version, reason, actor_kind, \
              observed_at, recorded_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'user', $10, $11)",
        ).bind(detail.journal_entry_id.into_uuid()).bind(detail.user_id.into_uuid())
         .bind(detail.account_id.into_uuid()).bind(detail.currency.as_str())
         .bind(detail.before_display_balance).bind(detail.target_display_balance)
         .bind(detail.display_delta).bind(detail.observed_balance_version).bind(detail.reason)
         .bind(detail.observed_at).bind(detail.recorded_at)
         .execute(&mut *self.transaction).await.map_err(LedgerError::database)?;
        Ok(())
    }
}

impl ProjectionStore for PgLedgerTransaction<'_> {
    async fn apply_postings(&mut self, journal: &JournalEntry) -> Result<(), LedgerError> {
        let mut account_ids: Vec<Uuid> = journal
            .postings()
            .iter()
            .map(|posting| posting.account_id().into_uuid())
            .collect();
        account_ids.sort_unstable();
        account_ids.dedup();
        sqlx::query(
            "SELECT account_id FROM ledger.account_balances \
             WHERE user_id = $1 AND account_id = ANY($2) ORDER BY account_id FOR UPDATE",
        )
        .bind(journal.user_id().into_uuid())
        .bind(&account_ids)
        .fetch_all(&mut *self.transaction)
        .await
        .map_err(LedgerError::database)?;

        for posting in journal.postings() {
            let updated = sqlx::query(
                "UPDATE ledger.account_balances \
                 SET signed_balance = signed_balance + $3, version = version + 1, as_of = $4 \
                 WHERE account_id = $1 AND user_id = $2",
            )
            .bind(posting.account_id().into_uuid())
            .bind(posting.user_id().into_uuid())
            .bind(posting.signed_amount())
            .bind(journal.recorded_at())
            .execute(&mut *self.transaction)
            .await
            .map_err(LedgerError::database)?;
            if updated.rows_affected() != 1 {
                return Err(LedgerError::not_found());
            }
        }
        Ok(())
    }

    async fn signed_balance(
        &mut self,
        user_id: UserId,
        account_id: LedgerAccountId,
        lock: bool,
    ) -> Result<Option<(Decimal, i64)>, LedgerError> {
        let sql = if lock {
            "SELECT signed_balance, version FROM ledger.account_balances \
             WHERE account_id = $1 AND user_id = $2 FOR UPDATE"
        } else {
            "SELECT signed_balance, version FROM ledger.account_balances \
             WHERE account_id = $1 AND user_id = $2"
        };
        sqlx::query_as(sql)
            .bind(account_id.into_uuid())
            .bind(user_id.into_uuid())
            .fetch_optional(&mut *self.transaction)
            .await
            .map_err(LedgerError::database)
    }
}

const RECONCILIATION_COLUMNS: &str =
    "id, user_id, account_id, observation_id, source_kind, source_stream_id, source_item_id, \
     observed_at, source_sequence, recorded_at, provider_reported_balance, available_balance, \
     currency, captured_ledger_balance, captured_balance_version, delta, status, version, \
     approval_journal_id, reason, decision_actor_kind, decision_actor_reference, created_at, updated_at";

impl ReconciliationStore for PgLedgerTransaction<'_> {
    async fn lock_reconciliation_stream(
        &mut self, user_id: UserId, account_id: LedgerAccountId, source: &SourceReference,
    ) -> Result<Option<ReconciliationStream>, LedgerError> {
        let identity = format!("{}:{}:{}:{}", user_id, account_id, source.source_kind(), source.stream_id());
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(identity).execute(&mut *self.transaction).await.map_err(LedgerError::database)?;
        #[derive(FromRow)]
        struct Row { latest_observed_at: chrono::DateTime<chrono::Utc>, latest_source_sequence: i64, latest_observation_id: Uuid, active_case_id: Option<Uuid> }
        sqlx::query_as::<_, Row>(
            "SELECT latest_observed_at, latest_source_sequence, latest_observation_id, active_case_id \
             FROM ledger.reconciliation_streams WHERE user_id = $1 AND account_id = $2 \
             AND source_kind = $3 AND source_stream_id = $4 FOR UPDATE",
        ).bind(user_id.into_uuid()).bind(account_id.into_uuid()).bind(source.source_kind())
         .bind(source.stream_id()).fetch_optional(&mut *self.transaction).await
         .map_err(LedgerError::database).map(|row| row.map(|row| ReconciliationStream {
             latest_observed_at: row.latest_observed_at, latest_source_sequence: row.latest_source_sequence,
             latest_observation_id: ObservationId::new(row.latest_observation_id),
             active_case_id: row.active_case_id.map(ReconciliationCaseId::new),
         }))
    }

    async fn find_reconciliation_by_observation(
        &mut self, user_id: UserId, observation_id: ObservationId,
    ) -> Result<Option<ReconciliationCase>, LedgerError> {
        sqlx::query_as::<_, ReconciliationRow>(&format!(
            "SELECT {RECONCILIATION_COLUMNS} FROM ledger.reconciliation_cases \
             WHERE user_id = $1 AND observation_id = $2"
        )).bind(user_id.into_uuid()).bind(observation_id.into_uuid())
          .fetch_optional(&mut *self.transaction).await.map_err(LedgerError::database)?
          .map(ReconciliationRow::into_domain).transpose()
    }

    async fn find_reconciliation_case(
        &mut self, user_id: UserId, case_id: ReconciliationCaseId, lock: bool,
    ) -> Result<Option<ReconciliationCase>, LedgerError> {
        let suffix = if lock { " FOR UPDATE" } else { "" };
        sqlx::query_as::<_, ReconciliationRow>(&format!(
            "SELECT {RECONCILIATION_COLUMNS} FROM ledger.reconciliation_cases \
             WHERE user_id = $1 AND id = $2{suffix}"
        )).bind(user_id.into_uuid()).bind(case_id.into_uuid())
          .fetch_optional(&mut *self.transaction).await.map_err(LedgerError::database)?
          .map(ReconciliationRow::into_domain).transpose()
    }

    async fn insert_reconciliation_case(&mut self, case: &ReconciliationCase) -> Result<(), LedgerError> {
        let observation = case.observation();
        let (actor_kind, actor_reference) = case.decision_actor().map(actor_columns).unwrap_or(("system", None));
        sqlx::query(
            "INSERT INTO ledger.reconciliation_cases \
             (id, user_id, account_id, observation_id, source_kind, source_stream_id, source_item_id, \
              observed_at, source_sequence, recorded_at, provider_reported_balance, available_balance, \
              currency, captured_ledger_balance, captured_balance_version, delta, status, version, \
              approval_journal_id, reason, decision_actor_kind, decision_actor_reference, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24)",
        ).bind(case.id().into_uuid()).bind(case.user_id().into_uuid()).bind(case.account_id().into_uuid())
         .bind(observation.id().into_uuid()).bind(observation.source().source_kind())
         .bind(observation.source().stream_id()).bind(observation.source().item_id())
         .bind(observation.observed_at()).bind(observation.source_sequence()).bind(observation.recorded_at())
         .bind(observation.provider_reported().amount()).bind(observation.available().map(Money::amount))
         .bind(observation.provider_reported().currency().as_str()).bind(case.captured_ledger_balance().amount())
         .bind(case.captured_balance_version().get()).bind(case.delta().amount()).bind(case.status().as_str())
         .bind(case.version().get()).bind(case.approval_journal_id().map(JournalEntryId::into_uuid))
         .bind(case.reason()).bind(actor_kind).bind(actor_reference).bind(case.created_at()).bind(case.updated_at())
         .execute(&mut *self.transaction).await.map_err(LedgerError::database)?;
        Ok(())
    }

    async fn save_reconciliation_case(&mut self, case: &ReconciliationCase) -> Result<(), LedgerError> {
        let (actor_kind, actor_reference) = case.decision_actor().map(actor_columns).unwrap_or(("system", None));
        let updated = sqlx::query(
            "UPDATE ledger.reconciliation_cases SET status=$3, version=$4, approval_journal_id=$5, \
             reason=$6, decision_actor_kind=$7, decision_actor_reference=$8, updated_at=$9 \
             WHERE id=$1 AND user_id=$2 AND version=$10",
        ).bind(case.id().into_uuid()).bind(case.user_id().into_uuid()).bind(case.status().as_str())
         .bind(case.version().get()).bind(case.approval_journal_id().map(JournalEntryId::into_uuid))
         .bind(case.reason()).bind(actor_kind).bind(actor_reference).bind(case.updated_at())
         .bind(case.version().get() - 1).execute(&mut *self.transaction).await.map_err(LedgerError::database)?;
        if updated.rows_affected() != 1 { return Err(LedgerError::version_conflict()) }
        Ok(())
    }

    async fn save_reconciliation_stream(
        &mut self, user_id: UserId, account_id: LedgerAccountId, source: &SourceReference,
        stream: &ReconciliationStream, now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), LedgerError> {
        sqlx::query(
            "INSERT INTO ledger.reconciliation_streams \
             (user_id, account_id, source_kind, source_stream_id, latest_observed_at, \
              latest_source_sequence, latest_observation_id, active_case_id, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) \
             ON CONFLICT (user_id, account_id, source_kind, source_stream_id) DO UPDATE SET \
              latest_observed_at=EXCLUDED.latest_observed_at, latest_source_sequence=EXCLUDED.latest_source_sequence, \
              latest_observation_id=EXCLUDED.latest_observation_id, active_case_id=EXCLUDED.active_case_id, updated_at=EXCLUDED.updated_at",
        ).bind(user_id.into_uuid()).bind(account_id.into_uuid()).bind(source.source_kind()).bind(source.stream_id())
         .bind(stream.latest_observed_at).bind(stream.latest_source_sequence)
         .bind(stream.latest_observation_id.into_uuid()).bind(stream.active_case_id.map(ReconciliationCaseId::into_uuid))
         .bind(now).execute(&mut *self.transaction).await.map_err(LedgerError::database)?;
        Ok(())
    }
}

impl CommandReceiptStore for PgLedgerTransaction<'_> {
    async fn find_receipt(
        &mut self,
        user_id: UserId,
        command_name: &str,
        key: &IdempotencyKey,
        lock: bool,
    ) -> Result<Option<StoredReceipt>, LedgerError> {
        #[derive(FromRow)]
        struct ReceiptRow { request_hash: Vec<u8>, status: String, result: serde_json::Value }
        let sql = if lock {
            "SELECT request_hash, status, result FROM ledger.command_receipts \
             WHERE user_id = $1 AND command_name = $2 AND idempotency_key = $3 FOR UPDATE"
        } else {
            "SELECT request_hash, status, result FROM ledger.command_receipts \
             WHERE user_id = $1 AND command_name = $2 AND idempotency_key = $3"
        };
        let row = sqlx::query_as::<_, ReceiptRow>(sql)
            .bind(user_id.into_uuid())
            .bind(command_name)
            .bind(key.as_str())
            .fetch_optional(&mut *self.transaction)
            .await
            .map_err(LedgerError::database)?;
        row.map(|row| {
            let request_hash = <[u8; 32]>::try_from(row.request_hash)
                .map_err(|_| LedgerError::persistence("stored receipt hash is invalid"))?;
            Ok(StoredReceipt { request_hash, status: row.status, result: row.result })
        }).transpose()
    }

    async fn insert_receipt(
        &mut self,
        user_id: UserId,
        command_name: &str,
        key: &IdempotencyKey,
        request_hash: &[u8; 32],
        result: &serde_json::Value,
        completed_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), LedgerError> {
        sqlx::query(
            "INSERT INTO ledger.command_receipts \
             (user_id, command_name, idempotency_key, request_hash, status, result, completed_at) \
             VALUES ($1, $2, $3, $4, 'completed', $5, $6)",
        )
        .bind(user_id.into_uuid())
        .bind(command_name)
        .bind(key.as_str())
        .bind(request_hash.as_slice())
        .bind(result)
        .bind(completed_at)
        .execute(&mut *self.transaction)
        .await
        .map_err(LedgerError::database)?;
        Ok(())
    }
}

impl AuditStore for PgLedgerTransaction<'_> {
    async fn append_audit(&mut self, record: &AuditRecord) -> Result<(), LedgerError> {
        sqlx::query(
            "INSERT INTO ledger.audit_events \
             (event_id, user_id, aggregate_kind, aggregate_id, event_type, actor_kind, actor_reference, \
              correlation_id, payload, occurred_at, recorded_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(record.event_id.into_uuid())
        .bind(record.user_id.into_uuid())
        .bind(record.aggregate_kind)
        .bind(record.aggregate_id)
        .bind(record.event_type)
        .bind(record.actor_kind)
        .bind(&record.actor_reference)
        .bind(record.correlation_id)
        .bind(&record.payload)
        .bind(record.occurred_at)
        .bind(record.recorded_at)
        .execute(&mut *self.transaction)
        .await
        .map_err(LedgerError::database)?;
        Ok(())
    }
}

impl LedgerOutboxStore for PgLedgerTransaction<'_> {
    async fn append_outbox(&mut self, event: &IntegrationEvent) -> Result<(), LedgerError> {
        PgOutboxWriter::from_transaction(&mut self.transaction)
            .append(event)
            .await
            .map_err(|error| LedgerError::persistence(error.to_string()))
    }
}

fn actor_columns(actor: &Actor) -> (&'static str, Option<String>) {
    match actor {
        Actor::User(user_id) => ("user", Some(user_id.to_string())),
        Actor::System => ("system", None),
        Actor::External { source_kind, source_reference } => {
            ("external", Some(format!("{source_kind}:{source_reference}")))
        }
    }
}
