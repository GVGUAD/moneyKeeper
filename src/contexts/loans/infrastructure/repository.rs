//! PostgreSQL Loans aggregate store and atomic unit of work.

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use crate::contexts::ledger::public::{JournalEntryId, LedgerAccountId};
use crate::contexts::loans::domain::{
    LoanAgreementId, LoanDirection, LoanMovementId, LoanStatus, MovementKind, MovementStatus,
};
use crate::contexts::loans::public::{
    LoanCommandResult, LoanEventFactV1, LoanEventMetadataV1, LoanEventV1, LoanMovementView,
    LoanView, MovementAmounts, OpenLoan, PendingLoanMovement, PendingLoanReplacement,
    PendingLoanReversal, RecordLoanMovement, RequestLoanReversal, ReviseLoanTerms,
};
use crate::shared_kernel::{CorrelationId, CurrencyCode, EventId, UserId};

#[derive(Debug, thiserror::Error)]
pub(crate) enum StoreError {
    #[error("loan was not found")]
    NotFound,
    #[error("loan version conflict")]
    VersionConflict,
    #[error("loan idempotency conflict")]
    IdempotencyConflict,
    #[error("loan command is invalid: {0}")]
    Invalid(&'static str),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

#[derive(Clone)]
pub(crate) struct PgLoansStore {
    pool: PgPool,
}

impl PgLoansStore {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub(crate) async fn open(
        &self,
        command: OpenLoan,
        hash: [u8; 32],
    ) -> Result<LoanCommandResult, StoreError> {
        validate_positive(command.contractual_principal)?;
        validate_dates(command.start_date, command.due_date)?;
        validate_counterparty(&command.counterparty)?;
        if command
            .annual_rate
            .is_some_and(|value| value.is_sign_negative())
        {
            return Err(StoreError::Invalid("annual_rate"));
        }
        let id = LoanAgreementId::generate();
        let now = command.occurred_at;
        let mut tx = self.pool.begin().await?;
        if let Some(value) = claim_receipt(
            &mut tx,
            command.user_id,
            "open_loan",
            command.idempotency_key.as_str(),
            hash,
        )
        .await?
        {
            tx.commit().await?;
            return serde_json::from_value(value).map_err(|_| StoreError::Invalid("stored_result"));
        }
        let direction = direction_str(command.direction);
        sqlx::query("INSERT INTO loans.agreements(id,user_id,direction,counterparty,contractual_principal,currency,start_date,due_date,annual_rate,status,version,created_at,updated_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,'pending_accounting',1,$10,$10)")
            .bind(id.into_uuid()).bind(command.user_id.into_uuid()).bind(direction)
            .bind(&command.counterparty).bind(command.contractual_principal)
            .bind(command.currency.as_str()).bind(command.start_date).bind(command.due_date)
            .bind(command.annual_rate).bind(now).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO loans.term_revisions(id,agreement_id,user_id,revision,counterparty,contractual_principal,start_date,due_date,annual_rate,reason,recorded_at) VALUES($1,$2,$3,1,$4,$5,$6,$7,$8,'Agreement opened',$9)")
            .bind(Uuid::new_v4()).bind(id.into_uuid()).bind(command.user_id.into_uuid())
            .bind(&command.counterparty).bind(command.contractual_principal).bind(command.start_date)
            .bind(command.due_date).bind(command.annual_rate).bind(now).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO loans.component_balances(agreement_id,user_id,currency,updated_at) VALUES($1,$2,$3,$4)")
            .bind(id.into_uuid()).bind(command.user_id.into_uuid()).bind(command.currency.as_str())
            .bind(now).execute(&mut *tx).await?;
        let result = LoanCommandResult {
            agreement_id: id,
            movement_id: None,
            status: "pending_accounting".to_owned(),
            version: 1,
            replayed: false,
        };
        append_event(
            &mut tx,
            command.user_id,
            id,
            1,
            command.correlation_id,
            LoanEventFactV1::AgreementOpened {
                agreement_id: id,
                direction: command.direction,
                counterparty: command.counterparty,
                contractual_principal: command.contractual_principal,
                currency: command.currency,
                start_date: command.start_date,
                due_date: command.due_date,
            },
        )
        .await?;
        finish_receipt(
            &mut tx,
            command.user_id,
            "open_loan",
            command.idempotency_key.as_str(),
            202,
            serde_json::to_value(&result).map_err(|_| StoreError::Invalid("result"))?,
            id.into_uuid(),
            1,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }

    pub(crate) async fn list(&self, user: UserId) -> Result<Vec<LoanView>, StoreError> {
        let rows = sqlx::query(LOAN_VIEW_SQL)
            .bind(user.into_uuid())
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(loan_view).collect()
    }

    pub(crate) async fn get(
        &self,
        user: UserId,
        id: LoanAgreementId,
    ) -> Result<Option<LoanView>, StoreError> {
        sqlx::query(LOAN_VIEW_SQL_ID)
            .bind(user.into_uuid())
            .bind(id.into_uuid())
            .fetch_optional(&self.pool)
            .await?
            .map(loan_view)
            .transpose()
    }

    pub(crate) async fn term_revisions(
        &self,
        user: UserId,
        id: LoanAgreementId,
    ) -> Result<Vec<Value>, StoreError> {
        Ok(sqlx::query("SELECT id,revision,counterparty,contractual_principal,start_date,due_date,annual_rate,reason,recorded_at FROM loans.term_revisions WHERE user_id=$1 AND agreement_id=$2 ORDER BY revision")
            .bind(user.into_uuid()).bind(id.into_uuid()).fetch_all(&self.pool).await?
            .into_iter().map(|row| json!({"id":row.get::<Uuid,_>("id"),"revision":row.get::<i64,_>("revision"),
                "counterparty":row.get::<String,_>("counterparty"),"contractual_principal":row.get::<Decimal,_>("contractual_principal").to_string(),
                "start_date":row.get::<NaiveDate,_>("start_date"),"due_date":row.get::<Option<NaiveDate>,_>("due_date"),
                "annual_rate":row.get::<Option<Decimal>,_>("annual_rate").map(|v|v.to_string()),"reason":row.get::<String,_>("reason"),
                "recorded_at":row.get::<DateTime<Utc>,_>("recorded_at")})).collect())
    }

    pub(crate) async fn movements(
        &self,
        user: UserId,
        id: LoanAgreementId,
    ) -> Result<Vec<LoanMovementView>, StoreError> {
        sqlx::query(MOVEMENT_SQL)
            .bind(user.into_uuid())
            .bind(id.into_uuid())
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(movement_view)
            .collect()
    }

    pub(crate) async fn movement(
        &self,
        user: UserId,
        agreement: LoanAgreementId,
        movement: LoanMovementId,
    ) -> Result<Option<LoanMovementView>, StoreError> {
        sqlx::query(&format!("{MOVEMENT_SQL} AND id=$3"))
            .bind(user.into_uuid())
            .bind(agreement.into_uuid())
            .bind(movement.into_uuid())
            .fetch_optional(&self.pool)
            .await?
            .as_ref()
            .map(movement_view)
            .transpose()
    }

    pub(crate) async fn revise(
        &self,
        command: ReviseLoanTerms,
        hash: [u8; 32],
    ) -> Result<LoanCommandResult, StoreError> {
        validate_positive(command.contractual_principal)?;
        validate_dates(command.start_date, command.due_date)?;
        validate_counterparty(&command.counterparty)?;
        if command.reason.is_empty() || command.reason.trim() != command.reason {
            return Err(StoreError::Invalid("reason"));
        }
        let mut tx = self.pool.begin().await?;
        if let Some(value) = claim_receipt(
            &mut tx,
            command.user_id,
            "revise_loan_terms",
            command.idempotency_key.as_str(),
            hash,
        )
        .await?
        {
            tx.commit().await?;
            return serde_json::from_value(value).map_err(|_| StoreError::Invalid("stored_result"));
        }
        let row=sqlx::query("SELECT currency,version FROM loans.agreements WHERE id=$1 AND user_id=$2 AND status IN ('pending_accounting','active') FOR UPDATE")
            .bind(command.agreement_id.into_uuid()).bind(command.user_id.into_uuid()).fetch_optional(&mut *tx).await?;
        let Some(row) = row else {
            return Err(StoreError::NotFound);
        };
        let version = row.get::<i64, _>("version");
        if version != i64::try_from(command.expected_version).unwrap_or(i64::MAX) {
            return Err(StoreError::VersionConflict);
        }
        if row.get::<String, _>("currency") != command.currency.as_str() {
            return Err(StoreError::Invalid("currency"));
        }
        let disbursed: Decimal=sqlx::query_scalar("SELECT COALESCE(SUM(principal),0) FROM loans.movements WHERE agreement_id=$1 AND user_id=$2 AND kind='disbursement' AND status='posted'")
            .bind(command.agreement_id.into_uuid()).bind(command.user_id.into_uuid()).fetch_one(&mut *tx).await?;
        if command.contractual_principal < disbursed {
            return Err(StoreError::Invalid("contractual_principal"));
        }
        let next = version + 1;
        sqlx::query("INSERT INTO loans.term_revisions(id,agreement_id,user_id,revision,counterparty,contractual_principal,start_date,due_date,annual_rate,reason,recorded_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)")
            .bind(Uuid::new_v4()).bind(command.agreement_id.into_uuid()).bind(command.user_id.into_uuid()).bind(next)
            .bind(&command.counterparty).bind(command.contractual_principal).bind(command.start_date).bind(command.due_date)
            .bind(command.annual_rate).bind(&command.reason).bind(command.occurred_at).execute(&mut *tx).await?;
        sqlx::query("UPDATE loans.agreements SET counterparty=$3,contractual_principal=$4,start_date=$5,due_date=$6,annual_rate=$7,version=$8,updated_at=$9 WHERE id=$1 AND user_id=$2")
            .bind(command.agreement_id.into_uuid()).bind(command.user_id.into_uuid()).bind(&command.counterparty)
            .bind(command.contractual_principal).bind(command.start_date).bind(command.due_date).bind(command.annual_rate)
            .bind(next).bind(command.occurred_at).execute(&mut *tx).await?;
        let result = LoanCommandResult {
            agreement_id: command.agreement_id,
            movement_id: None,
            status: "active".to_owned(),
            version: u64::try_from(next).unwrap_or_default(),
            replayed: false,
        };
        append_event(
            &mut tx,
            command.user_id,
            command.agreement_id,
            next,
            command.correlation_id,
            LoanEventFactV1::TermsRevised {
                agreement_id: command.agreement_id,
                revision: u64::try_from(next).unwrap_or_default(),
            },
        )
        .await?;
        finish_receipt(
            &mut tx,
            command.user_id,
            "revise_loan_terms",
            command.idempotency_key.as_str(),
            200,
            serde_json::to_value(&result).map_err(|_| StoreError::Invalid("result"))?,
            command.agreement_id.into_uuid(),
            next,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }

    pub(crate) async fn record_movement(
        &self,
        command: RecordLoanMovement,
        hash: [u8; 32],
    ) -> Result<LoanCommandResult, StoreError> {
        command.amounts.validate().map_err(StoreError::Invalid)?;
        let mut tx = self.pool.begin().await?;
        let scope = movement_scope(command.kind);
        if let Some(value) = claim_receipt(
            &mut tx,
            command.user_id,
            scope,
            command.idempotency_key.as_str(),
            hash,
        )
        .await?
        {
            tx.commit().await?;
            return serde_json::from_value(value).map_err(|_| StoreError::Invalid("stored_result"));
        }
        let agreement=sqlx::query("SELECT currency,contractual_principal,status,version FROM loans.agreements WHERE id=$1 AND user_id=$2 FOR UPDATE")
            .bind(command.agreement_id.into_uuid()).bind(command.user_id.into_uuid()).fetch_optional(&mut *tx).await?;
        let Some(agreement) = agreement else {
            return Err(StoreError::NotFound);
        };
        if agreement.get::<String, _>("status") != "active" {
            return Err(StoreError::Invalid("agreement_status"));
        }
        let version = agreement.get::<i64, _>("version");
        if version != i64::try_from(command.expected_version).unwrap_or(i64::MAX) {
            return Err(StoreError::VersionConflict);
        }
        if agreement.get::<String, _>("currency") != command.currency.as_str() {
            return Err(StoreError::Invalid("currency"));
        }
        validate_movement_shape(
            command.kind,
            &command.amounts,
            command.cash_account_id,
            command.reason.as_deref(),
        )?;
        let balances=sqlx::query("SELECT principal,accrued_interest,accrued_fee FROM loans.component_balances WHERE agreement_id=$1 AND user_id=$2 FOR UPDATE")
            .bind(command.agreement_id.into_uuid()).bind(command.user_id.into_uuid()).fetch_one(&mut *tx).await?;
        let outstanding = MovementAmounts {
            principal: balances.get("principal"),
            accrued_interest: balances.get("accrued_interest"),
            accrued_fee: balances.get("accrued_fee"),
            current_interest: Decimal::ZERO,
            current_fee: Decimal::ZERO,
        };
        if matches!(
            command.kind,
            MovementKind::Repayment | MovementKind::WriteOff
        ) && (command.amounts.principal > outstanding.principal
            || command.amounts.accrued_interest > outstanding.accrued_interest
            || command.amounts.accrued_fee > outstanding.accrued_fee)
        {
            return Err(StoreError::Invalid("component_overpayment"));
        }
        if command.kind == MovementKind::Disbursement {
            let posted:Decimal=sqlx::query_scalar("SELECT COALESCE(SUM(principal),0) FROM loans.movements WHERE agreement_id=$1 AND user_id=$2 AND kind='disbursement' AND status='posted'")
                .bind(command.agreement_id.into_uuid()).bind(command.user_id.into_uuid()).fetch_one(&mut *tx).await?;
            let pending:Decimal=sqlx::query_scalar("SELECT COALESCE(SUM(principal),0) FROM loans.movements WHERE agreement_id=$1 AND user_id=$2 AND kind='disbursement' AND status='pending_accounting'")
                .bind(command.agreement_id.into_uuid()).bind(command.user_id.into_uuid()).fetch_one(&mut *tx).await?;
            if posted + pending + command.amounts.principal
                > agreement.get::<Decimal, _>("contractual_principal")
            {
                return Err(StoreError::Invalid("contractual_principal_exceeded"));
            }
        }
        let movement = LoanMovementId::generate();
        let next = version + 1;
        let sequence: i64=sqlx::query_scalar("SELECT COALESCE(MAX(sequence),0)+1 FROM loans.movements WHERE agreement_id=$1 AND user_id=$2")
            .bind(command.agreement_id.into_uuid()).bind(command.user_id.into_uuid()).fetch_one(&mut *tx).await?;
        sqlx::query("INSERT INTO loans.movements(id,agreement_id,user_id,sequence,kind,currency,principal,accrued_interest,accrued_fee,current_interest,current_fee,cash_account_id,reason,status,process_correlation_id,replaces_movement_id,requested_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,'pending_accounting',$14,$15,$16)")
            .bind(movement.into_uuid()).bind(command.agreement_id.into_uuid()).bind(command.user_id.into_uuid()).bind(sequence)
            .bind(kind_str(command.kind)).bind(command.currency.as_str()).bind(command.amounts.principal)
            .bind(command.amounts.accrued_interest).bind(command.amounts.accrued_fee).bind(command.amounts.current_interest)
            .bind(command.amounts.current_fee).bind(command.cash_account_id.map(LedgerAccountId::into_uuid)).bind(command.reason)
            .bind(command.correlation_id.into_uuid()).bind(command.replaces.map(LoanMovementId::into_uuid)).bind(command.occurred_at)
            .execute(&mut *tx).await?;
        sqlx::query("INSERT INTO loans.movement_status_history(movement_id,user_id,sequence,status,recorded_at) VALUES($1,$2,1,'pending_accounting',$3)")
            .bind(movement.into_uuid()).bind(command.user_id.into_uuid()).bind(command.occurred_at).execute(&mut *tx).await?;
        sqlx::query(
            "UPDATE loans.agreements SET version=$3,updated_at=$4 WHERE id=$1 AND user_id=$2",
        )
        .bind(command.agreement_id.into_uuid())
        .bind(command.user_id.into_uuid())
        .bind(next)
        .bind(command.occurred_at)
        .execute(&mut *tx)
        .await?;
        let result = LoanCommandResult {
            agreement_id: command.agreement_id,
            movement_id: Some(movement),
            status: "pending_accounting".to_owned(),
            version: u64::try_from(next).unwrap_or_default(),
            replayed: false,
        };
        append_event(
            &mut tx,
            command.user_id,
            command.agreement_id,
            next,
            command.correlation_id,
            LoanEventFactV1::MovementRequested {
                agreement_id: command.agreement_id,
                movement_id: movement,
                kind: command.kind,
                amounts: command.amounts,
            },
        )
        .await?;
        finish_receipt(
            &mut tx,
            command.user_id,
            scope,
            command.idempotency_key.as_str(),
            202,
            serde_json::to_value(&result).map_err(|_| StoreError::Invalid("result"))?,
            command.agreement_id.into_uuid(),
            next,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn close(
        &self,
        user: UserId,
        id: LoanAgreementId,
        expected: u64,
        key: &str,
        hash: [u8; 32],
        correlation: CorrelationId,
        now: DateTime<Utc>,
    ) -> Result<LoanCommandResult, StoreError> {
        let mut tx = self.pool.begin().await?;
        if let Some(value) = claim_receipt(&mut tx, user, "close_loan", key, hash).await? {
            tx.commit().await?;
            return serde_json::from_value(value).map_err(|_| StoreError::Invalid("stored_result"));
        }
        let row=sqlx::query("SELECT a.version,a.status,b.principal,b.accrued_interest,b.accrued_fee,(EXISTS(SELECT 1 FROM loans.movements m WHERE m.agreement_id=a.id AND m.user_id=a.user_id AND m.status IN ('replacement_requested','pending_accounting','reversal_pending')) OR EXISTS(SELECT 1 FROM loans.replacement_processes p JOIN loans.movements replacement ON replacement.id=p.replacement_movement_id AND replacement.user_id=p.user_id WHERE replacement.agreement_id=a.id AND p.user_id=a.user_id AND p.state NOT IN ('posted','terminal_failure')) OR EXISTS(SELECT 1 FROM loans.reversal_requests r WHERE r.agreement_id=a.id AND r.user_id=a.user_id AND r.state NOT IN ('posted','terminal_failure'))) AS pending FROM loans.agreements a JOIN loans.component_balances b ON b.agreement_id=a.id AND b.user_id=a.user_id WHERE a.id=$1 AND a.user_id=$2 FOR UPDATE OF a,b")
            .bind(id.into_uuid()).bind(user.into_uuid()).fetch_optional(&mut *tx).await?;
        let Some(row) = row else {
            return Err(StoreError::NotFound);
        };
        let version = row.get::<i64, _>("version");
        if version != i64::try_from(expected).unwrap_or(i64::MAX) {
            return Err(StoreError::VersionConflict);
        }
        if row.get::<String, _>("status") != "active"
            || row.get::<bool, _>("pending")
            || !["principal", "accrued_interest", "accrued_fee"]
                .into_iter()
                .all(|c| row.get::<Decimal, _>(c).is_zero())
        {
            return Err(StoreError::Invalid("loan_cannot_close"));
        }
        let next = version + 1;
        sqlx::query("UPDATE loans.agreements SET status='closed',version=$3,updated_at=$4 WHERE id=$1 AND user_id=$2")
            .bind(id.into_uuid()).bind(user.into_uuid()).bind(next).bind(now).execute(&mut *tx).await?;
        let result = LoanCommandResult {
            agreement_id: id,
            movement_id: None,
            status: "closed".to_owned(),
            version: u64::try_from(next).unwrap_or_default(),
            replayed: false,
        };
        append_event(
            &mut tx,
            user,
            id,
            next,
            correlation,
            LoanEventFactV1::AgreementClosed { agreement_id: id },
        )
        .await?;
        finish_receipt(
            &mut tx,
            user,
            "close_loan",
            key,
            200,
            serde_json::to_value(&result).map_err(|_| StoreError::Invalid("result"))?,
            id.into_uuid(),
            next,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }

    pub(crate) async fn pending_openings(&self, limit: i64) -> Result<Vec<LoanView>, StoreError> {
        let rows=sqlx::query(&format!("{LOAN_VIEW_SQL_ALL} WHERE a.status='pending_accounting' ORDER BY a.created_at,a.id LIMIT $1"))
            .bind(limit).fetch_all(&self.pool).await?;
        rows.into_iter().map(loan_view).collect()
    }
    pub(crate) async fn confirm_opening(
        &self,
        user: UserId,
        id: LoanAgreementId,
        account: LedgerAccountId,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let result=sqlx::query("UPDATE loans.agreements SET ledger_principal_account_id=$3,status='active',accounting_error=NULL,version=version+1,updated_at=$4 WHERE id=$1 AND user_id=$2 AND status='pending_accounting'")
            .bind(id.into_uuid()).bind(user.into_uuid()).bind(account.into_uuid()).bind(now).execute(&self.pool).await?;
        if result.rows_affected() == 0 {
            return Err(StoreError::VersionConflict);
        }
        Ok(())
    }
    pub(crate) async fn fail_opening(
        &self,
        user: UserId,
        id: LoanAgreementId,
        error: &str,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        sqlx::query("UPDATE loans.agreements SET status='failed',accounting_error=$3,version=version+1,updated_at=$4 WHERE id=$1 AND user_id=$2 AND status='pending_accounting'")
            .bind(id.into_uuid()).bind(user.into_uuid()).bind(error).bind(now).execute(&self.pool).await?;
        Ok(())
    }
    pub(crate) async fn pending_movements(
        &self,
        limit: i64,
    ) -> Result<Vec<PendingLoanMovement>, StoreError> {
        let rows=sqlx::query("SELECT m.*,a.direction,a.ledger_principal_account_id,a.version AS agreement_version FROM loans.movements m JOIN loans.agreements a ON a.id=m.agreement_id AND a.user_id=m.user_id WHERE m.status='pending_accounting' ORDER BY m.requested_at,m.id LIMIT $1")
            .bind(limit).fetch_all(&self.pool).await?;
        rows.iter().map(pending_movement).collect()
    }
    pub(crate) async fn confirm_movement(
        &self,
        user: UserId,
        agreement: LoanAgreementId,
        movement: LoanMovementId,
        journal: JournalEntryId,
        now: DateTime<Utc>,
    ) -> Result<LoanEventV1, StoreError> {
        let mut tx = self.pool.begin().await?;
        let row=sqlx::query("SELECT kind,principal,accrued_interest,accrued_fee,process_correlation_id FROM loans.movements WHERE id=$1 AND agreement_id=$2 AND user_id=$3 AND status='pending_accounting' FOR UPDATE")
            .bind(movement.into_uuid()).bind(agreement.into_uuid()).bind(user.into_uuid()).fetch_optional(&mut *tx).await?;
        let Some(row) = row else {
            return Err(StoreError::VersionConflict);
        };
        let kind = parse_kind(&row.get::<String, _>("kind"))?;
        let sign = if matches!(kind, MovementKind::Disbursement | MovementKind::Accrual) {
            Decimal::ONE
        } else {
            -Decimal::ONE
        };
        let principal = row.get::<Decimal, _>("principal") * sign;
        let interest = row.get::<Decimal, _>("accrued_interest") * sign;
        let fee = row.get::<Decimal, _>("accrued_fee") * sign;
        let balances=sqlx::query("UPDATE loans.component_balances SET principal=principal+$3,accrued_interest=accrued_interest+$4,accrued_fee=accrued_fee+$5,version=version+1,updated_at=$6 WHERE agreement_id=$1 AND user_id=$2 RETURNING currency,principal,accrued_interest,accrued_fee,version")
            .bind(agreement.into_uuid()).bind(user.into_uuid()).bind(principal).bind(interest).bind(fee).bind(now).fetch_one(&mut *tx).await?;
        sqlx::query("UPDATE loans.movements SET status='posted',ledger_journal_id=$4,posted_at=$5,last_error=NULL WHERE id=$1 AND agreement_id=$2 AND user_id=$3")
            .bind(movement.into_uuid()).bind(agreement.into_uuid()).bind(user.into_uuid()).bind(journal.into_uuid()).bind(now).execute(&mut *tx).await?;
        sqlx::query("UPDATE loans.replacement_processes SET state='posted',version=version+1,updated_at=$3 WHERE replacement_movement_id=$1 AND user_id=$2 AND state='posting_replacement'")
            .bind(movement.into_uuid()).bind(user.into_uuid()).bind(now).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO loans.movement_status_history(movement_id,user_id,sequence,status,ledger_journal_id,recorded_at) SELECT $1,$2,COALESCE(MAX(sequence),0)+1,'posted',$3,$4 FROM loans.movement_status_history WHERE movement_id=$1 AND user_id=$2")
            .bind(movement.into_uuid()).bind(user.into_uuid()).bind(journal.into_uuid()).bind(now).execute(&mut *tx).await?;
        let version:i64=sqlx::query_scalar("UPDATE loans.agreements SET version=version+1,updated_at=$3 WHERE id=$1 AND user_id=$2 RETURNING version")
            .bind(agreement.into_uuid()).bind(user.into_uuid()).bind(now).fetch_one(&mut *tx).await?;
        let event = LoanEventV1 {
            metadata: LoanEventMetadataV1 {
                schema_version: 1,
                event_id: EventId::generate(),
                user_id: user,
                sequence: u64::try_from(version).unwrap_or_default(),
                correlation_id: CorrelationId::new(row.get("process_correlation_id")),
                occurred_at: now,
            },
            fact: LoanEventFactV1::MovementPosted {
                agreement_id: agreement,
                movement_id: movement,
                kind,
                balances: MovementAmounts {
                    principal: balances.get("principal"),
                    accrued_interest: balances.get("accrued_interest"),
                    accrued_fee: balances.get("accrued_fee"),
                    current_interest: Decimal::ZERO,
                    current_fee: Decimal::ZERO,
                },
                ledger_journal_id: journal,
            },
        };
        append_existing_event(&mut tx, agreement, version, &event).await?;
        tx.commit().await?;
        Ok(event)
    }
    pub(crate) async fn fail_movement(
        &self,
        user: UserId,
        agreement: LoanAgreementId,
        movement: LoanMovementId,
        error: &str,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        let changed=sqlx::query("UPDATE loans.movements SET status='failed',last_error=$4 WHERE id=$1 AND agreement_id=$2 AND user_id=$3 AND status='pending_accounting'")
            .bind(movement.into_uuid()).bind(agreement.into_uuid()).bind(user.into_uuid()).bind(error).execute(&mut *tx).await?;
        if changed.rows_affected() == 0 {
            return Err(StoreError::VersionConflict);
        }
        sqlx::query("INSERT INTO loans.movement_status_history(movement_id,user_id,sequence,status,error,recorded_at) SELECT $1,$2,COALESCE(MAX(sequence),0)+1,'failed',$3,$4 FROM loans.movement_status_history WHERE movement_id=$1 AND user_id=$2")
            .bind(movement.into_uuid()).bind(user.into_uuid()).bind(error).bind(now).execute(&mut *tx).await?;
        sqlx::query("UPDATE loans.agreements SET version=version+1,updated_at=$3 WHERE id=$1 AND user_id=$2")
            .bind(agreement.into_uuid()).bind(user.into_uuid()).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub(crate) async fn request_reversal(
        &self,
        command: RequestLoanReversal,
        hash: [u8; 32],
    ) -> Result<LoanCommandResult, StoreError> {
        if command.reason.is_empty() || command.reason.trim() != command.reason {
            return Err(StoreError::Invalid("reason"));
        }
        let mut tx = self.pool.begin().await?;
        if let Some(value) = claim_receipt(
            &mut tx,
            command.user_id,
            "reverse_loan_movement",
            command.idempotency_key.as_str(),
            hash,
        )
        .await?
        {
            tx.commit().await?;
            return serde_json::from_value(value).map_err(|_| StoreError::Invalid("stored_result"));
        }
        let agreement_version:Option<i64>=sqlx::query_scalar("SELECT version FROM loans.agreements WHERE id=$1 AND user_id=$2 AND status='active' FOR UPDATE")
            .bind(command.agreement_id.into_uuid()).bind(command.user_id.into_uuid()).fetch_optional(&mut *tx).await?;
        let Some(version) = agreement_version else {
            return Err(StoreError::NotFound);
        };
        if version != i64::try_from(command.expected_version).unwrap_or(i64::MAX) {
            return Err(StoreError::VersionConflict);
        }
        let changed=sqlx::query("UPDATE loans.movements SET status='reversal_pending' WHERE id=$1 AND agreement_id=$2 AND user_id=$3 AND status='posted'")
            .bind(command.movement_id.into_uuid()).bind(command.agreement_id.into_uuid()).bind(command.user_id.into_uuid()).execute(&mut *tx).await?;
        if changed.rows_affected() == 0 {
            return Err(StoreError::Invalid(
                "movement_not_posted_or_already_reversed",
            ));
        }
        sqlx::query("INSERT INTO loans.reversal_requests(movement_id,agreement_id,user_id,reason,correlation_id,state,requested_at) VALUES($1,$2,$3,$4,$5,'pending',$6)")
            .bind(command.movement_id.into_uuid()).bind(command.agreement_id.into_uuid()).bind(command.user_id.into_uuid()).bind(&command.reason).bind(command.correlation_id.into_uuid()).bind(command.occurred_at).execute(&mut *tx).await?;
        let next = version + 1;
        sqlx::query(
            "UPDATE loans.agreements SET version=$3,updated_at=$4 WHERE id=$1 AND user_id=$2",
        )
        .bind(command.agreement_id.into_uuid())
        .bind(command.user_id.into_uuid())
        .bind(next)
        .bind(command.occurred_at)
        .execute(&mut *tx)
        .await?;
        let result = LoanCommandResult {
            agreement_id: command.agreement_id,
            movement_id: Some(command.movement_id),
            status: "pending".to_owned(),
            version: u64::try_from(next).unwrap_or_default(),
            replayed: false,
        };
        finish_receipt(
            &mut tx,
            command.user_id,
            "reverse_loan_movement",
            command.idempotency_key.as_str(),
            202,
            serde_json::to_value(&result).map_err(|_| StoreError::Invalid("result"))?,
            command.agreement_id.into_uuid(),
            next,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }

    pub(crate) async fn pending_reversals(
        &self,
        limit: i64,
    ) -> Result<Vec<PendingLoanReversal>, StoreError> {
        let rows=sqlx::query("SELECT r.reason,r.correlation_id,m.*,a.direction,a.ledger_principal_account_id,a.version AS agreement_version FROM loans.reversal_requests r JOIN loans.movements m ON m.id=r.movement_id AND m.user_id=r.user_id JOIN loans.agreements a ON a.id=r.agreement_id AND a.user_id=r.user_id WHERE r.state IN ('pending','retry_due') ORDER BY r.requested_at,r.movement_id LIMIT $1")
            .bind(limit).fetch_all(&self.pool).await?;
        rows.iter()
            .map(|row| {
                Ok(PendingLoanReversal {
                    pending: pending_movement(row)?,
                    reason: row.get("reason"),
                })
            })
            .collect()
    }

    pub(crate) async fn confirm_reversal(
        &self,
        p: &PendingLoanReversal,
        reversal: JournalEntryId,
        now: DateTime<Utc>,
    ) -> Result<LoanEventV1, StoreError> {
        let m = &p.pending.movement;
        let mut tx = self.pool.begin().await?;
        let sign = if matches!(m.kind, MovementKind::Disbursement | MovementKind::Accrual) {
            -Decimal::ONE
        } else {
            Decimal::ONE
        };
        let balances=sqlx::query("UPDATE loans.component_balances SET principal=principal+$3,accrued_interest=accrued_interest+$4,accrued_fee=accrued_fee+$5,version=version+1,updated_at=$6 WHERE agreement_id=$1 AND user_id=$2 RETURNING principal,accrued_interest,accrued_fee")
            .bind(m.agreement_id.into_uuid()).bind(p.pending.user_id.into_uuid()).bind(m.amounts.principal*sign).bind(m.amounts.accrued_interest*sign).bind(m.amounts.accrued_fee*sign).bind(now).fetch_one(&mut *tx).await?;
        sqlx::query("UPDATE loans.movements SET status='reversed',ledger_reversal_id=$4,reversed_at=$5 WHERE id=$1 AND agreement_id=$2 AND user_id=$3 AND status='reversal_pending'")
            .bind(m.id.into_uuid()).bind(m.agreement_id.into_uuid()).bind(p.pending.user_id.into_uuid()).bind(reversal.into_uuid()).bind(now).execute(&mut *tx).await?;
        sqlx::query("UPDATE loans.reversal_requests SET state='posted',ledger_reversal_id=$3,completed_at=$4 WHERE movement_id=$1 AND user_id=$2")
            .bind(m.id.into_uuid()).bind(p.pending.user_id.into_uuid()).bind(reversal.into_uuid()).bind(now).execute(&mut *tx).await?;
        let version:i64=sqlx::query_scalar("UPDATE loans.agreements SET version=version+1,updated_at=$3 WHERE id=$1 AND user_id=$2 RETURNING version")
            .bind(m.agreement_id.into_uuid()).bind(p.pending.user_id.into_uuid()).bind(now).fetch_one(&mut *tx).await?;
        let event = LoanEventV1 {
            metadata: LoanEventMetadataV1 {
                schema_version: 1,
                event_id: EventId::generate(),
                user_id: p.pending.user_id,
                sequence: u64::try_from(version).unwrap_or_default(),
                correlation_id: m.correlation_id,
                occurred_at: now,
            },
            fact: LoanEventFactV1::MovementReversed {
                agreement_id: m.agreement_id,
                movement_id: m.id,
                balances: MovementAmounts {
                    principal: balances.get("principal"),
                    accrued_interest: balances.get("accrued_interest"),
                    accrued_fee: balances.get("accrued_fee"),
                    current_interest: Decimal::ZERO,
                    current_fee: Decimal::ZERO,
                },
                ledger_reversal_id: reversal,
            },
        };
        append_existing_event(&mut tx, m.agreement_id, version, &event).await?;
        tx.commit().await?;
        Ok(event)
    }

    pub(crate) async fn request_replacement(
        &self,
        mut command: RecordLoanMovement,
        original: LoanMovementId,
        hash: [u8; 32],
    ) -> Result<LoanCommandResult, StoreError> {
        command.amounts.validate().map_err(StoreError::Invalid)?;
        validate_movement_shape(
            command.kind,
            &command.amounts,
            command.cash_account_id,
            command.reason.as_deref(),
        )?;
        let mut tx = self.pool.begin().await?;
        if let Some(value) = claim_receipt(
            &mut tx,
            command.user_id,
            "replace_loan_movement",
            command.idempotency_key.as_str(),
            hash,
        )
        .await?
        {
            tx.commit().await?;
            return serde_json::from_value(value).map_err(|_| StoreError::Invalid("stored_result"));
        }
        let version:Option<i64>=sqlx::query_scalar("SELECT version FROM loans.agreements WHERE id=$1 AND user_id=$2 AND status='active' FOR UPDATE")
            .bind(command.agreement_id.into_uuid()).bind(command.user_id.into_uuid()).fetch_optional(&mut *tx).await?;
        let Some(version) = version else {
            return Err(StoreError::NotFound);
        };
        if version != i64::try_from(command.expected_version).unwrap_or(i64::MAX) {
            return Err(StoreError::VersionConflict);
        }
        let original_status:Option<String>=sqlx::query_scalar("SELECT status FROM loans.movements WHERE id=$1 AND agreement_id=$2 AND user_id=$3 FOR UPDATE")
            .bind(original.into_uuid()).bind(command.agreement_id.into_uuid()).bind(command.user_id.into_uuid()).fetch_optional(&mut *tx).await?;
        if original_status.as_deref() != Some("posted") {
            return Err(StoreError::Invalid(
                "movement_not_posted_or_already_replaced",
            ));
        }
        let movement = LoanMovementId::generate();
        let sequence:i64=sqlx::query_scalar("SELECT COALESCE(MAX(sequence),0)+1 FROM loans.movements WHERE agreement_id=$1 AND user_id=$2")
            .bind(command.agreement_id.into_uuid()).bind(command.user_id.into_uuid()).fetch_one(&mut *tx).await?;
        command.replaces = Some(original);
        sqlx::query("INSERT INTO loans.movements(id,agreement_id,user_id,sequence,kind,currency,principal,accrued_interest,accrued_fee,current_interest,current_fee,cash_account_id,reason,status,process_correlation_id,replaces_movement_id,requested_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,'replacement_requested',$14,$15,$16)")
            .bind(movement.into_uuid()).bind(command.agreement_id.into_uuid()).bind(command.user_id.into_uuid()).bind(sequence).bind(kind_str(command.kind)).bind(command.currency.as_str()).bind(command.amounts.principal).bind(command.amounts.accrued_interest).bind(command.amounts.accrued_fee).bind(command.amounts.current_interest).bind(command.amounts.current_fee).bind(command.cash_account_id.map(LedgerAccountId::into_uuid)).bind(command.reason).bind(command.correlation_id.into_uuid()).bind(original.into_uuid()).bind(command.occurred_at).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO loans.movement_status_history(movement_id,user_id,sequence,status,recorded_at) VALUES($1,$2,1,'replacement_requested',$3)")
            .bind(movement.into_uuid()).bind(command.user_id.into_uuid()).bind(command.occurred_at).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO loans.replacement_processes(original_movement_id,replacement_movement_id,user_id,state,correlation_id,version,updated_at) VALUES($1,$2,$3,'replacement_requested',$4,1,$5)")
            .bind(original.into_uuid()).bind(movement.into_uuid()).bind(command.user_id.into_uuid()).bind(command.correlation_id.into_uuid()).bind(command.occurred_at).execute(&mut *tx).await?;
        let next = version + 1;
        sqlx::query(
            "UPDATE loans.agreements SET version=$3,updated_at=$4 WHERE id=$1 AND user_id=$2",
        )
        .bind(command.agreement_id.into_uuid())
        .bind(command.user_id.into_uuid())
        .bind(next)
        .bind(command.occurred_at)
        .execute(&mut *tx)
        .await?;
        let result = LoanCommandResult {
            agreement_id: command.agreement_id,
            movement_id: Some(movement),
            status: "replacement_requested".to_owned(),
            version: u64::try_from(next).unwrap_or_default(),
            replayed: false,
        };
        finish_receipt(
            &mut tx,
            command.user_id,
            "replace_loan_movement",
            command.idempotency_key.as_str(),
            202,
            serde_json::to_value(&result).map_err(|_| StoreError::Invalid("result"))?,
            command.agreement_id.into_uuid(),
            next,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }

    pub(crate) async fn pending_replacements(
        &self,
        limit: i64,
    ) -> Result<Vec<PendingLoanReplacement>, StoreError> {
        let rows=sqlx::query("SELECT p.original_movement_id,p.replacement_movement_id,p.correlation_id,p.state,m.*,a.direction,a.ledger_principal_account_id,a.version AS agreement_version,o.ledger_journal_id AS original_journal_id FROM loans.replacement_processes p JOIN loans.movements m ON m.id=p.replacement_movement_id AND m.user_id=p.user_id JOIN loans.movements o ON o.id=p.original_movement_id AND o.user_id=p.user_id JOIN loans.agreements a ON a.id=m.agreement_id AND a.user_id=m.user_id WHERE p.state IN ('replacement_requested','retry_due') ORDER BY p.updated_at,p.replacement_movement_id LIMIT $1")
            .bind(limit).fetch_all(&self.pool).await?;
        rows.iter()
            .map(|row| {
                Ok(PendingLoanReplacement {
                    replacement: pending_movement(row)?,
                    original_movement_id: LoanMovementId::new(row.get("original_movement_id")),
                    original_journal_id: JournalEntryId::new(row.get("original_journal_id")),
                })
            })
            .collect()
    }

    pub(crate) async fn confirm_replacement_reversal(
        &self,
        p: &PendingLoanReplacement,
        reversal: JournalEntryId,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let replacement = &p.replacement.movement;
        let mut tx = self.pool.begin().await?;
        let original=sqlx::query("SELECT kind,principal,accrued_interest,accrued_fee FROM loans.movements WHERE id=$1 AND user_id=$2 AND status='posted' FOR UPDATE")
            .bind(p.original_movement_id.into_uuid()).bind(p.replacement.user_id.into_uuid()).fetch_optional(&mut *tx).await?;
        let Some(original) = original else {
            return Err(StoreError::VersionConflict);
        };
        let kind = parse_kind(&original.get::<String, _>("kind"))?;
        let sign = if matches!(kind, MovementKind::Disbursement | MovementKind::Accrual) {
            -Decimal::ONE
        } else {
            Decimal::ONE
        };
        sqlx::query("UPDATE loans.component_balances SET principal=principal+$3,accrued_interest=accrued_interest+$4,accrued_fee=accrued_fee+$5,version=version+1,updated_at=$6 WHERE agreement_id=$1 AND user_id=$2")
            .bind(replacement.agreement_id.into_uuid()).bind(p.replacement.user_id.into_uuid()).bind(original.get::<Decimal,_>("principal")*sign).bind(original.get::<Decimal,_>("accrued_interest")*sign).bind(original.get::<Decimal,_>("accrued_fee")*sign).bind(now).execute(&mut *tx).await?;
        sqlx::query("UPDATE loans.movements SET status='reversed',ledger_reversal_id=$3,reversed_by_movement_id=$4,reversed_at=$5 WHERE id=$1 AND user_id=$2")
            .bind(p.original_movement_id.into_uuid()).bind(p.replacement.user_id.into_uuid()).bind(reversal.into_uuid()).bind(replacement.id.into_uuid()).bind(now).execute(&mut *tx).await?;
        sqlx::query("UPDATE loans.movements SET status='pending_accounting' WHERE id=$1 AND user_id=$2 AND status='replacement_requested'")
            .bind(replacement.id.into_uuid()).bind(p.replacement.user_id.into_uuid()).execute(&mut *tx).await?;
        sqlx::query("UPDATE loans.replacement_processes SET state='posting_replacement',version=version+1,updated_at=$4 WHERE original_movement_id=$1 AND replacement_movement_id=$2 AND user_id=$3")
            .bind(p.original_movement_id.into_uuid()).bind(replacement.id.into_uuid()).bind(p.replacement.user_id.into_uuid()).bind(now).execute(&mut *tx).await?;
        sqlx::query("UPDATE loans.agreements SET version=version+1,updated_at=$3 WHERE id=$1 AND user_id=$2")
            .bind(replacement.agreement_id.into_uuid()).bind(p.replacement.user_id.into_uuid()).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
}

const LOAN_VIEW_SQL_ALL: &str = "SELECT a.*,b.principal,b.accrued_interest,b.accrued_fee,b.version AS balance_version FROM loans.agreements a JOIN loans.component_balances b ON b.agreement_id=a.id AND b.user_id=a.user_id";
const LOAN_VIEW_SQL: &str = "SELECT a.*,b.principal,b.accrued_interest,b.accrued_fee,b.version AS balance_version FROM loans.agreements a JOIN loans.component_balances b ON b.agreement_id=a.id AND b.user_id=a.user_id WHERE a.user_id=$1 ORDER BY a.updated_at DESC,a.id";
const LOAN_VIEW_SQL_ID: &str = "SELECT a.*,b.principal,b.accrued_interest,b.accrued_fee,b.version AS balance_version FROM loans.agreements a JOIN loans.component_balances b ON b.agreement_id=a.id AND b.user_id=a.user_id WHERE a.user_id=$1 AND a.id=$2";
const MOVEMENT_SQL: &str = "SELECT * FROM loans.movements WHERE user_id=$1 AND agreement_id=$2";

fn loan_view(row: sqlx::postgres::PgRow) -> Result<LoanView, StoreError> {
    Ok(LoanView {
        id: LoanAgreementId::new(row.get("id")),
        user_id: UserId::new(row.get("user_id")),
        direction: parse_direction(&row.get::<String, _>("direction"))?,
        counterparty: row.get("counterparty"),
        contractual_principal: row.get("contractual_principal"),
        currency: CurrencyCode::new(row.get::<String, _>("currency"))
            .map_err(|_| StoreError::Invalid("stored_currency"))?,
        start_date: row.get("start_date"),
        due_date: row.get("due_date"),
        annual_rate: row.get("annual_rate"),
        ledger_principal_account_id: row
            .get::<Option<Uuid>, _>("ledger_principal_account_id")
            .map(LedgerAccountId::new),
        status: parse_status(&row.get::<String, _>("status"))?,
        balances: MovementAmounts {
            principal: row.get("principal"),
            accrued_interest: row.get("accrued_interest"),
            accrued_fee: row.get("accrued_fee"),
            current_interest: Decimal::ZERO,
            current_fee: Decimal::ZERO,
        },
        version: u64::try_from(row.get::<i64, _>("version")).unwrap_or_default(),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    })
}
fn movement_view(row: &sqlx::postgres::PgRow) -> Result<LoanMovementView, StoreError> {
    Ok(LoanMovementView {
        id: LoanMovementId::new(row.get("id")),
        agreement_id: LoanAgreementId::new(row.get("agreement_id")),
        kind: parse_kind(&row.get::<String, _>("kind"))?,
        currency: CurrencyCode::new(row.get::<String, _>("currency"))
            .map_err(|_| StoreError::Invalid("stored_currency"))?,
        amounts: MovementAmounts {
            principal: row.get("principal"),
            accrued_interest: row.get("accrued_interest"),
            accrued_fee: row.get("accrued_fee"),
            current_interest: row.get("current_interest"),
            current_fee: row.get("current_fee"),
        },
        cash_account_id: row
            .get::<Option<Uuid>, _>("cash_account_id")
            .map(LedgerAccountId::new),
        reason: row.get("reason"),
        status: parse_movement_status(&row.get::<String, _>("status"))?,
        correlation_id: CorrelationId::new(row.get("process_correlation_id")),
        ledger_journal_id: row
            .get::<Option<Uuid>, _>("ledger_journal_id")
            .map(JournalEntryId::new),
        ledger_reversal_id: row
            .get::<Option<Uuid>, _>("ledger_reversal_id")
            .map(JournalEntryId::new),
        replaces: row
            .get::<Option<Uuid>, _>("replaces_movement_id")
            .map(LoanMovementId::new),
        requested_at: row.get("requested_at"),
    })
}
fn pending_movement(row: &sqlx::postgres::PgRow) -> Result<PendingLoanMovement, StoreError> {
    Ok(PendingLoanMovement {
        movement: movement_view(row)?,
        user_id: UserId::new(row.get("user_id")),
        direction: parse_direction(&row.get::<String, _>("direction"))?,
        principal_account_id: LedgerAccountId::new(row.get("ledger_principal_account_id")),
        agreement_version: u64::try_from(row.get::<i64, _>("agreement_version"))
            .unwrap_or_default(),
    })
}
fn direction_str(v: LoanDirection) -> &'static str {
    match v {
        LoanDirection::Borrowed => "borrowed",
        LoanDirection::Lent => "lent",
    }
}
fn kind_str(v: MovementKind) -> &'static str {
    match v {
        MovementKind::Disbursement => "disbursement",
        MovementKind::Repayment => "repayment",
        MovementKind::Accrual => "accrual",
        MovementKind::WriteOff => "write_off",
    }
}
fn movement_scope(v: MovementKind) -> &'static str {
    match v {
        MovementKind::Disbursement => "record_disbursement",
        MovementKind::Repayment => "record_repayment",
        MovementKind::Accrual => "record_interest_accrual",
        MovementKind::WriteOff => "record_write_off",
    }
}
fn parse_direction(v: &str) -> Result<LoanDirection, StoreError> {
    match v {
        "borrowed" => Ok(LoanDirection::Borrowed),
        "lent" => Ok(LoanDirection::Lent),
        _ => Err(StoreError::Invalid("stored_direction")),
    }
}
fn parse_kind(v: &str) -> Result<MovementKind, StoreError> {
    match v {
        "disbursement" => Ok(MovementKind::Disbursement),
        "repayment" => Ok(MovementKind::Repayment),
        "accrual" => Ok(MovementKind::Accrual),
        "write_off" => Ok(MovementKind::WriteOff),
        _ => Err(StoreError::Invalid("stored_kind")),
    }
}
fn parse_status(v: &str) -> Result<LoanStatus, StoreError> {
    match v {
        "draft" => Ok(LoanStatus::Draft),
        "pending_accounting" => Ok(LoanStatus::PendingAccounting),
        "active" => Ok(LoanStatus::Active),
        "failed" => Ok(LoanStatus::Failed),
        "closed" => Ok(LoanStatus::Closed),
        _ => Err(StoreError::Invalid("stored_status")),
    }
}
fn parse_movement_status(v: &str) -> Result<MovementStatus, StoreError> {
    match v {
        "replacement_requested" => Ok(MovementStatus::ReplacementRequested),
        "pending_accounting" => Ok(MovementStatus::PendingAccounting),
        "posted" => Ok(MovementStatus::Posted),
        "failed" => Ok(MovementStatus::Failed),
        "reversal_pending" => Ok(MovementStatus::ReversalPending),
        "reversed" => Ok(MovementStatus::Reversed),
        _ => Err(StoreError::Invalid("stored_status")),
    }
}
fn validate_positive(value: Decimal) -> Result<(), StoreError> {
    if value <= Decimal::ZERO {
        Err(StoreError::Invalid("amount"))
    } else {
        Ok(())
    }
}
fn validate_dates(start: NaiveDate, due: Option<NaiveDate>) -> Result<(), StoreError> {
    if due.is_some_and(|v| v < start) {
        Err(StoreError::Invalid("due_date"))
    } else {
        Ok(())
    }
}
fn validate_counterparty(v: &str) -> Result<(), StoreError> {
    if v.is_empty() || v.trim() != v || v.len() > 200 || v.chars().any(char::is_control) {
        Err(StoreError::Invalid("counterparty"))
    } else {
        Ok(())
    }
}
fn validate_movement_shape(
    kind: MovementKind,
    a: &MovementAmounts,
    cash: Option<LedgerAccountId>,
    reason: Option<&str>,
) -> Result<(), StoreError> {
    if matches!(kind, MovementKind::Disbursement | MovementKind::Repayment) != cash.is_some() {
        return Err(StoreError::Invalid("cash_account_id"));
    }
    if kind == MovementKind::Disbursement
        && (a.principal.is_zero()
            || !a.accrued_interest.is_zero()
            || !a.accrued_fee.is_zero()
            || !a.current_interest.is_zero()
            || !a.current_fee.is_zero())
    {
        return Err(StoreError::Invalid("disbursement_components"));
    }
    if kind == MovementKind::Accrual
        && (!a.principal.is_zero() || !a.current_interest.is_zero() || !a.current_fee.is_zero())
    {
        return Err(StoreError::Invalid("accrual_components"));
    }
    if kind == MovementKind::WriteOff
        && (reason.is_none_or(|v| v.is_empty() || v.trim() != v)
            || !a.current_interest.is_zero()
            || !a.current_fee.is_zero())
    {
        return Err(StoreError::Invalid("write_off"));
    }
    Ok(())
}

async fn claim_receipt(
    tx: &mut Transaction<'_, Postgres>,
    user: UserId,
    scope: &str,
    key: &str,
    hash: [u8; 32],
) -> Result<Option<Value>, StoreError> {
    let inserted=sqlx::query("INSERT INTO loans.command_receipts(user_id,command_scope,idempotency_key,canonical_request_hash,status_code,durable_result,aggregate_id,aggregate_version,created_at,completed_at) VALUES($1,$2,$3,$4,102,'null'::jsonb,'00000000-0000-0000-0000-000000000000',1,$5,$5) ON CONFLICT DO NOTHING")
        .bind(user.into_uuid()).bind(scope).bind(key).bind(hash.as_slice()).bind(Utc::now()).execute(&mut **tx).await?;
    if inserted.rows_affected() == 1 {
        return Ok(None);
    }
    let row=sqlx::query("SELECT canonical_request_hash,status_code,durable_result FROM loans.command_receipts WHERE user_id=$1 AND command_scope=$2 AND idempotency_key=$3")
        .bind(user.into_uuid()).bind(scope).bind(key).fetch_one(&mut **tx).await?;
    if row.get::<Vec<u8>, _>("canonical_request_hash") != hash {
        return Err(StoreError::IdempotencyConflict);
    }
    if row.get::<i16, _>("status_code") == 102 {
        return Err(StoreError::VersionConflict);
    }
    Ok(Some(row.get("durable_result")))
}
#[allow(clippy::too_many_arguments)]
async fn finish_receipt(
    tx: &mut Transaction<'_, Postgres>,
    user: UserId,
    scope: &str,
    key: &str,
    status: i16,
    result: Value,
    aggregate: Uuid,
    version: i64,
) -> Result<(), StoreError> {
    sqlx::query("UPDATE loans.command_receipts SET status_code=$4,durable_result=$5,aggregate_id=$6,aggregate_version=$7,completed_at=$8 WHERE user_id=$1 AND command_scope=$2 AND idempotency_key=$3")
        .bind(user.into_uuid()).bind(scope).bind(key).bind(status).bind(result).bind(aggregate).bind(version).bind(Utc::now()).execute(&mut **tx).await?;
    Ok(())
}
async fn append_event(
    tx: &mut Transaction<'_, Postgres>,
    user: UserId,
    agreement: LoanAgreementId,
    version: i64,
    correlation: CorrelationId,
    fact: LoanEventFactV1,
) -> Result<(), StoreError> {
    let event = LoanEventV1 {
        metadata: LoanEventMetadataV1 {
            schema_version: 1,
            event_id: EventId::generate(),
            user_id: user,
            sequence: u64::try_from(version).unwrap_or_default(),
            correlation_id: correlation,
            occurred_at: Utc::now(),
        },
        fact,
    };
    append_existing_event(tx, agreement, version, &event).await
}
async fn append_existing_event(
    tx: &mut Transaction<'_, Postgres>,
    agreement: LoanAgreementId,
    version: i64,
    event: &LoanEventV1,
) -> Result<(), StoreError> {
    let event_type = event.event_type();
    sqlx::query("INSERT INTO integration.outbox_messages(message_id,event_id,message_schema_version,context_name,aggregate_id,aggregate_version,event_type,user_id,occurred_at,correlation_id,payload) VALUES($1,$2,1,'loans',$3,$4,$5,$6,$7,$8,$9)")
        .bind(Uuid::new_v4()).bind(event.metadata.event_id.into_uuid()).bind(agreement.to_string()).bind(version).bind(event_type).bind(event.metadata.user_id.into_uuid()).bind(event.metadata.occurred_at).bind(event.metadata.correlation_id.into_uuid()).bind(serde_json::to_value(event).map_err(|_|StoreError::Invalid("event"))?).execute(&mut **tx).await?;
    Ok(())
}
