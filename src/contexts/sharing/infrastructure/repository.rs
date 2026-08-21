//! PostgreSQL aggregate repositories and atomic Sharing unit of work.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use crate::contexts::sharing::{
    application::{commands::*, queries::*},
    domain::*,
};
use crate::shared_kernel::{CurrencyCode, Money, UserId};

#[derive(Clone)]
pub(crate) struct PgSharingStore {
    pool: PgPool,
}

impl PgSharingStore {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub(crate) async fn create_contact(
        &self,
        command: CreateContact,
    ) -> Result<ContactResult, SharingError> {
        let mut tx = self.pool.begin().await.map_err(database)?;
        if let Some(result) = replay(
            &mut tx,
            command.metadata.user_id,
            "create_contact",
            &command.metadata,
        )
        .await?
        {
            return Ok(result);
        }
        let contact = Contact::create(
            ContactId::generate(),
            command.metadata.user_id,
            command.name,
            command.note,
        )?;
        sqlx::query("INSERT INTO sharing.contacts(id,user_id,display_name,note,lifecycle,version,created_at,updated_at) VALUES($1,$2,$3,$4,'active',1,$5,$5)")
            .bind(contact.id().into_uuid()).bind(contact.user_id().into_uuid()).bind(contact.name().as_str()).bind(contact.note()).bind(command.metadata.occurred_at).execute(&mut *tx).await.map_err(database)?;
        let result = ContactResult {
            contact: ContactView::from(&contact),
            replayed: false,
        };
        audit(
            &mut tx,
            contact.user_id(),
            "contact",
            contact.id().into_uuid(),
            1,
            "created",
            command.metadata.correlation_id.into_uuid(),
        )
        .await?;
        save_receipt(
            &mut tx,
            contact.user_id(),
            "create_contact",
            &command.metadata,
            201,
            &result,
        )
        .await?;
        tx.commit().await.map_err(database)?;
        Ok(result)
    }

    pub(crate) async fn update_contact(
        &self,
        command: UpdateContact,
    ) -> Result<ContactResult, SharingError> {
        let mut tx = self.pool.begin().await.map_err(database)?;
        if let Some(result) = replay(
            &mut tx,
            command.metadata.user_id,
            "update_contact",
            &command.metadata,
        )
        .await?
        {
            return Ok(result);
        }
        let mut contact =
            load_contact_for_update(&mut tx, command.metadata.user_id, command.contact_id)
                .await?
                .ok_or(SharingError::NotFound)?;
        contact.edit(command.name, command.note, command.expected_version)?;
        persist_contact(&mut tx, &contact, command.expected_version).await?;
        let result = ContactResult {
            contact: ContactView::from(&contact),
            replayed: false,
        };
        audit(
            &mut tx,
            contact.user_id(),
            "contact",
            contact.id().into_uuid(),
            contact.version().0,
            "updated",
            command.metadata.correlation_id.into_uuid(),
        )
        .await?;
        save_receipt(
            &mut tx,
            contact.user_id(),
            "update_contact",
            &command.metadata,
            200,
            &result,
        )
        .await?;
        tx.commit().await.map_err(database)?;
        Ok(result)
    }

    pub(crate) async fn archive_contact(
        &self,
        command: ArchiveContact,
    ) -> Result<ContactResult, SharingError> {
        let mut tx = self.pool.begin().await.map_err(database)?;
        if let Some(result) = replay(
            &mut tx,
            command.metadata.user_id,
            "archive_contact",
            &command.metadata,
        )
        .await?
        {
            return Ok(result);
        }
        let mut contact =
            load_contact_for_update(&mut tx, command.metadata.user_id, command.contact_id)
                .await?
                .ok_or(SharingError::NotFound)?;
        contact.archive(command.expected_version)?;
        persist_contact(&mut tx, &contact, command.expected_version).await?;
        let result = ContactResult {
            contact: ContactView::from(&contact),
            replayed: false,
        };
        audit(
            &mut tx,
            contact.user_id(),
            "contact",
            contact.id().into_uuid(),
            contact.version().0,
            "archived",
            command.metadata.correlation_id.into_uuid(),
        )
        .await?;
        save_receipt(
            &mut tx,
            contact.user_id(),
            "archive_contact",
            &command.metadata,
            200,
            &result,
        )
        .await?;
        tx.commit().await.map_err(database)?;
        Ok(result)
    }

    pub(crate) async fn contact(
        &self,
        user: UserId,
        id: ContactId,
    ) -> Result<Option<ContactView>, SharingError> {
        load_contact(&self.pool, user, id)
            .await
            .map(|value| value.as_ref().map(ContactView::from))
    }
    pub(crate) async fn contacts(
        &self,
        user: UserId,
        include_archived: bool,
    ) -> Result<Vec<ContactView>, SharingError> {
        let rows = sqlx::query("SELECT id,user_id,display_name,note,lifecycle,version FROM sharing.contacts WHERE user_id=$1 AND ($2 OR lifecycle='active') ORDER BY lower(display_name),id")
            .bind(user.into_uuid()).bind(include_archived).fetch_all(&self.pool).await.map_err(database)?;
        rows.into_iter()
            .map(row_to_contact)
            .map(|value| value.map(|contact| ContactView::from(&contact)))
            .collect()
    }

    pub(crate) async fn create_bill(
        &self,
        command: CreateBillSplit,
    ) -> Result<BillResult, SharingError> {
        let mut tx = self.pool.begin().await.map_err(database)?;
        if let Some(result) = replay(
            &mut tx,
            command.metadata.user_id,
            "create_bill",
            &command.metadata,
        )
        .await?
        {
            return Ok(result);
        }
        validate_contacts(&mut tx, command.metadata.user_id, &command.draft).await?;
        let shares = resolve_allocations(
            &command.draft.total,
            &command.draft.contributions,
            command.draft.shares.clone(),
            command.draft.minor_unit_scale,
        )?;
        let obligations = derive_obligations(
            &command.draft.contributions,
            &shares,
            command.draft.minor_unit_scale,
        )?;
        let revision = BillRevision::new(
            1,
            &command.draft.title,
            command.draft.occurred_at,
            command.draft.total,
            command.draft.contributions,
            shares,
            obligations,
            command.metadata.correlation_id,
        )?;
        let bill = BillSplit::create(BillSplitId::generate(), command.metadata.user_id, revision)?;
        insert_bill(&mut tx, &bill).await?;
        let view = bill_view_from_domain(&bill)?;
        create_process(
            &mut tx,
            "sharing_bill_accounting",
            &format!("{}:1", bill.id()),
            command.metadata.correlation_id,
            json!({"bill_id":bill.id(),"revision":1}),
        )
        .await?;
        append_event(
            &mut tx,
            &command.metadata,
            bill.id(),
            1,
            crate::contexts::sharing::public::ACCOUNTING_REQUESTED_V1,
            json!({"bill_id":bill.id(),"revision":1}),
        )
        .await?;
        let result = BillResult {
            bill: view,
            process: ProcessView {
                state: "pending_accounting".into(),
                correlation_id: command.metadata.correlation_id,
                last_error: None,
            },
            replayed: false,
        };
        audit(
            &mut tx,
            command.metadata.user_id,
            "bill_split",
            bill.id().into_uuid(),
            1,
            "created",
            command.metadata.correlation_id.into_uuid(),
        )
        .await?;
        save_receipt(
            &mut tx,
            command.metadata.user_id,
            "create_bill",
            &command.metadata,
            202,
            &result,
        )
        .await?;
        tx.commit().await.map_err(database)?;
        Ok(result)
    }

    pub(crate) async fn revise_bill(
        &self,
        command: ReviseBillSplit,
    ) -> Result<BillResult, SharingError> {
        let mut tx = self.pool.begin().await.map_err(database)?;
        if let Some(result) = replay(
            &mut tx,
            command.metadata.user_id,
            "revise_bill",
            &command.metadata,
        )
        .await?
        {
            return Ok(result);
        }
        let row = lock_bill(&mut tx, command.metadata.user_id, command.bill_id).await?;
        require_bill_version(&row, command.expected_version)?;
        let status: String = row.get("status");
        let active: i32 = row.get("active_settlements");
        if active > 0 {
            return Err(SharingError::ActiveSettlements);
        }
        if status == "pending_accounting" {
            return Err(SharingError::AccountingPending);
        }
        if !matches!(status.as_str(), "active" | "failed") {
            return Err(SharingError::InvalidTransition);
        }
        if row.get::<String, _>("currency") != command.draft.total.currency().as_str() {
            return Err(SharingError::CurrencyMismatch);
        }
        validate_contacts(&mut tx, command.metadata.user_id, &command.draft).await?;
        let shares = resolve_allocations(
            &command.draft.total,
            &command.draft.contributions,
            command.draft.shares.clone(),
            command.draft.minor_unit_scale,
        )?;
        let obligations = derive_obligations(
            &command.draft.contributions,
            &shares,
            command.draft.minor_unit_scale,
        )?;
        let revision_number = u32::try_from(row.get::<i32, _>("current_revision") + 1)
            .map_err(|_| SharingError::ArithmeticOverflow)?;
        let revision = BillRevision::new(
            revision_number,
            command.draft.title,
            command.draft.occurred_at,
            command.draft.total,
            command.draft.contributions,
            shares,
            obligations,
            command.metadata.correlation_id,
        )?;
        insert_revision(
            &mut tx,
            command.bill_id,
            command.metadata.user_id,
            &revision,
        )
        .await?;
        let new_version = command.expected_version.0 + 1;
        sqlx::query("UPDATE sharing.bills SET current_revision=$1,status='pending_accounting',version=$2,updated_at=$3 WHERE id=$4 AND user_id=$5")
            .bind(i32::try_from(revision_number).map_err(|_| SharingError::ArithmeticOverflow)?).bind(i64::try_from(new_version).map_err(|_| SharingError::ArithmeticOverflow)?).bind(command.metadata.occurred_at).bind(command.bill_id.into_uuid()).bind(command.metadata.user_id.into_uuid()).execute(&mut *tx).await.map_err(database)?;
        create_process(&mut tx, "sharing_bill_accounting", &format!("{}:{revision_number}", command.bill_id), command.metadata.correlation_id, json!({"bill_id":command.bill_id,"revision":revision_number,"reverse_revision":revision_number-1})).await?;
        append_event(
            &mut tx,
            &command.metadata,
            command.bill_id,
            new_version,
            crate::contexts::sharing::public::ACCOUNTING_REQUESTED_V1,
            json!({"bill_id":command.bill_id,"revision":revision_number}),
        )
        .await?;
        let view = load_bill_tx(&mut tx, command.metadata.user_id, command.bill_id)
            .await?
            .ok_or(SharingError::NotFound)?;
        let result = BillResult {
            bill: view,
            process: ProcessView {
                state: "pending_accounting".into(),
                correlation_id: command.metadata.correlation_id,
                last_error: None,
            },
            replayed: false,
        };
        save_receipt(
            &mut tx,
            command.metadata.user_id,
            "revise_bill",
            &command.metadata,
            202,
            &result,
        )
        .await?;
        tx.commit().await.map_err(database)?;
        Ok(result)
    }

    pub(crate) async fn cancel_bill(
        &self,
        command: CancelBillSplit,
    ) -> Result<BillResult, SharingError> {
        let mut tx = self.pool.begin().await.map_err(database)?;
        if let Some(result) = replay(
            &mut tx,
            command.metadata.user_id,
            "cancel_bill",
            &command.metadata,
        )
        .await?
        {
            return Ok(result);
        }
        let row = lock_bill(&mut tx, command.metadata.user_id, command.bill_id).await?;
        require_bill_version(&row, command.expected_version)?;
        if row.get::<i32, _>("active_settlements") > 0 {
            return Err(SharingError::ActiveSettlements);
        }
        let status: String = row.get("status");
        if status == "pending_accounting" {
            return Err(SharingError::AccountingPending);
        }
        if !matches!(status.as_str(), "active" | "failed") {
            return Err(SharingError::InvalidTransition);
        }
        let reason = command.reason.trim();
        if reason.is_empty() {
            return Err(SharingError::Empty("cancellation reason"));
        }
        let new_version = command.expected_version.0 + 1;
        sqlx::query("UPDATE sharing.bills SET status='pending_cancellation',cancellation_reason=$1,version=$2,updated_at=$3 WHERE id=$4 AND user_id=$5").bind(reason).bind(i64::try_from(new_version).map_err(|_| SharingError::ArithmeticOverflow)?).bind(command.metadata.occurred_at).bind(command.bill_id.into_uuid()).bind(command.metadata.user_id.into_uuid()).execute(&mut *tx).await.map_err(database)?;
        create_process(
            &mut tx,
            "sharing_bill_cancellation",
            &command.bill_id.to_string(),
            command.metadata.correlation_id,
            json!({"bill_id":command.bill_id,"reason":reason}),
        )
        .await?;
        let view = load_bill_tx(&mut tx, command.metadata.user_id, command.bill_id)
            .await?
            .ok_or(SharingError::NotFound)?;
        let result = BillResult {
            bill: view,
            process: ProcessView {
                state: "pending_cancellation".into(),
                correlation_id: command.metadata.correlation_id,
                last_error: None,
            },
            replayed: false,
        };
        save_receipt(
            &mut tx,
            command.metadata.user_id,
            "cancel_bill",
            &command.metadata,
            202,
            &result,
        )
        .await?;
        tx.commit().await.map_err(database)?;
        Ok(result)
    }

    pub(crate) async fn bill(
        &self,
        user: UserId,
        id: BillSplitId,
    ) -> Result<Option<BillView>, SharingError> {
        let mut tx = self.pool.begin().await.map_err(database)?;
        let value = load_bill_tx(&mut tx, user, id).await?;
        tx.rollback().await.ok();
        Ok(value)
    }
    pub(crate) async fn bills(&self, user: UserId) -> Result<Vec<BillView>, SharingError> {
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM sharing.bills WHERE user_id=$1 ORDER BY created_at DESC,id",
        )
        .bind(user.into_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(database)?;
        let mut result = Vec::with_capacity(ids.len());
        let mut tx = self.pool.begin().await.map_err(database)?;
        for id in ids {
            if let Some(value) = load_bill_tx(&mut tx, user, BillSplitId::new(id)).await? {
                result.push(value);
            }
        }
        tx.rollback().await.ok();
        Ok(result)
    }

    pub(crate) async fn create_settlement(
        &self,
        command: CreateSettlement,
    ) -> Result<SettlementResult, SharingError> {
        let mut tx = self.pool.begin().await.map_err(database)?;
        if let Some(result) = replay(
            &mut tx,
            command.metadata.user_id,
            "create_settlement",
            &command.metadata,
        )
        .await?
        {
            return Ok(result);
        }
        let bill = lock_bill(&mut tx, command.metadata.user_id, command.bill_id).await?;
        require_bill_version(&bill, command.expected_version)?;
        if bill.get::<String, _>("status") != "active" {
            return Err(SharingError::InvalidTransition);
        }
        let (debtor_kind, debtor_id) = participant_db(command.debtor);
        let (creditor_kind, creditor_id) = participant_db(command.creditor);
        let obligation = sqlx::query("SELECT id,original_amount-settled_amount AS remaining,currency FROM sharing.obligations WHERE bill_id=$1 AND user_id=$2 AND revision=$3 AND debtor_kind=$4 AND debtor_contact_id IS NOT DISTINCT FROM $5 AND creditor_kind=$6 AND creditor_contact_id IS NOT DISTINCT FROM $7 FOR UPDATE")
            .bind(command.bill_id.into_uuid()).bind(command.metadata.user_id.into_uuid()).bind(bill.get::<i32,_>("current_revision")).bind(debtor_kind).bind(debtor_id).bind(creditor_kind).bind(creditor_id).fetch_optional(&mut *tx).await.map_err(database)?.ok_or(SharingError::NotFound)?;
        let currency = CurrencyCode::new(obligation.get::<String, _>("currency"))
            .map_err(|error| SharingError::Persistence(error.to_string()))?;
        let remaining = Money::new(
            obligation.get("remaining"),
            currency.clone(),
            command.amount.amount().scale(),
        )?;
        let settlement = Settlement::create(
            SettlementId::generate(),
            command.bill_id,
            command.metadata.user_id,
            command.debtor,
            command.creditor,
            command.amount,
            &remaining,
            command.evidence,
            command.metadata.occurred_at,
        )?;
        let (evidence_kind, account_id, journal_id) = settlement_evidence_db(settlement.evidence());
        sqlx::query("INSERT INTO sharing.settlements(id,bill_id,user_id,obligation_id,amount,currency,evidence_kind,ledger_account_id,ledger_journal_id,status,version,accounting_correlation_id,occurred_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,'pending_accounting',1,$10,$11)")
            .bind(settlement.id().into_uuid()).bind(command.bill_id.into_uuid()).bind(command.metadata.user_id.into_uuid()).bind(obligation.get::<Uuid,_>("id")).bind(settlement.amount().amount()).bind(currency.as_str()).bind(evidence_kind).bind(account_id).bind(journal_id).bind(command.metadata.correlation_id.into_uuid()).bind(command.metadata.occurred_at).execute(&mut *tx).await.map_err(database)?;
        sqlx::query("UPDATE sharing.obligations SET settled_amount=settled_amount+$1 WHERE id=$2 AND user_id=$3 AND settled_amount+$1<=original_amount")
            .bind(settlement.amount().amount()).bind(obligation.get::<Uuid,_>("id")).bind(command.metadata.user_id.into_uuid()).execute(&mut *tx).await.map_err(database)?;
        sqlx::query("UPDATE sharing.bills SET active_settlements=active_settlements+1,version=version+1,updated_at=$1 WHERE id=$2 AND user_id=$3").bind(command.metadata.occurred_at).bind(command.bill_id.into_uuid()).bind(command.metadata.user_id.into_uuid()).execute(&mut *tx).await.map_err(database)?;
        create_process(
            &mut tx,
            "sharing_settlement",
            &settlement.id().to_string(),
            command.metadata.correlation_id,
            json!({"settlement_id":settlement.id(),"bill_id":command.bill_id}),
        )
        .await?;
        append_event(
            &mut tx,
            &command.metadata,
            command.bill_id,
            command.expected_version.0 + 1,
            crate::contexts::sharing::public::SETTLEMENT_ACCOUNTING_REQUESTED_V1,
            json!({"settlement_id":settlement.id(),"bill_id":command.bill_id}),
        )
        .await?;
        let view = SettlementView {
            id: settlement.id(),
            bill_id: command.bill_id,
            amount: settlement.amount().amount(),
            currency,
            status: SettlementStatus::PendingAccounting,
            version: SettlementVersion(1),
            process: ProcessView {
                state: "pending_accounting".into(),
                correlation_id: command.metadata.correlation_id,
                last_error: None,
            },
        };
        let result = SettlementResult {
            settlement: view,
            replayed: false,
        };
        save_receipt(
            &mut tx,
            command.metadata.user_id,
            "create_settlement",
            &command.metadata,
            202,
            &result,
        )
        .await?;
        tx.commit().await.map_err(database)?;
        Ok(result)
    }

    pub(crate) async fn reverse_settlement(
        &self,
        command: ReverseSettlement,
    ) -> Result<SettlementResult, SharingError> {
        let mut tx = self.pool.begin().await.map_err(database)?;
        if let Some(result) = replay(
            &mut tx,
            command.metadata.user_id,
            "reverse_settlement",
            &command.metadata,
        )
        .await?
        {
            return Ok(result);
        }
        let row = sqlx::query("SELECT id,bill_id,amount,currency,status,version,accounting_correlation_id FROM sharing.settlements WHERE id=$1 AND bill_id=$2 AND user_id=$3 FOR UPDATE").bind(command.settlement_id.into_uuid()).bind(command.bill_id.into_uuid()).bind(command.metadata.user_id.into_uuid()).fetch_optional(&mut *tx).await.map_err(database)?.ok_or(SharingError::NotFound)?;
        let actual = u64::try_from(row.get::<i64, _>("version"))
            .map_err(|_| SharingError::ArithmeticOverflow)?;
        if actual != command.expected_version.0 {
            return Err(SharingError::VersionConflict {
                expected: command.expected_version.0,
                actual,
            });
        }
        let status: String = row.get("status");
        if status == "pending_accounting" {
            return Err(SharingError::AccountingPending);
        }
        let reason = command.reason.trim();
        if reason.is_empty() {
            return Err(SharingError::Empty("reversal reason"));
        }
        sqlx::query("INSERT INTO sharing.settlement_reversals(settlement_id,user_id,reason,correlation_id,reversed_at) VALUES($1,$2,$3,$4,$5)").bind(command.settlement_id.into_uuid()).bind(command.metadata.user_id.into_uuid()).bind(reason).bind(command.metadata.correlation_id.into_uuid()).bind(command.metadata.occurred_at).execute(&mut *tx).await.map_err(database)?;
        create_process(
            &mut tx,
            "sharing_settlement_reversal",
            &command.settlement_id.to_string(),
            command.metadata.correlation_id,
            json!({"settlement_id":command.settlement_id,"bill_id":command.bill_id}),
        )
        .await?;
        let currency = CurrencyCode::new(row.get::<String, _>("currency"))
            .map_err(|error| SharingError::Persistence(error.to_string()))?;
        let view = SettlementView {
            id: command.settlement_id,
            bill_id: command.bill_id,
            amount: row.get("amount"),
            currency,
            status: SettlementStatus::Reversed,
            version: SettlementVersion(actual + 1),
            process: ProcessView {
                state: "pending_reversal".into(),
                correlation_id: command.metadata.correlation_id,
                last_error: None,
            },
        };
        let result = SettlementResult {
            settlement: view,
            replayed: false,
        };
        save_receipt(
            &mut tx,
            command.metadata.user_id,
            "reverse_settlement",
            &command.metadata,
            202,
            &result,
        )
        .await?;
        tx.commit().await.map_err(database)?;
        Ok(result)
    }

    pub(crate) async fn complete_bill_accounting(
        &self,
        command: CompleteBillAccounting,
    ) -> Result<BillView, SharingError> {
        let mut tx = self.pool.begin().await.map_err(database)?;
        let row = lock_bill(&mut tx, command.user_id, command.bill_id).await?;
        require_bill_version(&row, command.expected_version)?;
        let current_revision = u32::try_from(row.get::<i32, _>("current_revision"))
            .map_err(|_| SharingError::ArithmeticOverflow)?;
        if row.get::<String, _>("status") != "pending_accounting"
            || current_revision != command.revision
        {
            return Err(SharingError::InvalidTransition);
        }
        sqlx::query("UPDATE sharing.bill_revisions SET accounting_status='posted',accounting_journal_id=$1,last_error=NULL WHERE bill_id=$2 AND user_id=$3 AND revision=$4")
            .bind(command.journal_id).bind(command.bill_id.into_uuid()).bind(command.user_id.into_uuid()).bind(i32::try_from(command.revision).map_err(|_|SharingError::ArithmeticOverflow)?).execute(&mut *tx).await.map_err(database)?;
        let version = command.expected_version.0 + 1;
        sqlx::query("UPDATE sharing.bills SET status='active',version=$1,updated_at=$2 WHERE id=$3 AND user_id=$4")
            .bind(i64::try_from(version).map_err(|_|SharingError::ArithmeticOverflow)?).bind(command.occurred_at).bind(command.bill_id.into_uuid()).bind(command.user_id.into_uuid()).execute(&mut *tx).await.map_err(database)?;
        sqlx::query("UPDATE integration.process_instances SET status='posted',version=version+1,next_wake_at=NULL,updated_at=clock_timestamp() WHERE process_name='sharing_bill_accounting' AND instance_key=$1")
            .bind(format!("{}:{}",command.bill_id,command.revision)).execute(&mut *tx).await.map_err(database)?;
        let payload =
            current_position(&mut tx, command.user_id, command.bill_id, command.revision).await?;
        let metadata = system_metadata(
            command.user_id,
            command.correlation_id,
            command.occurred_at,
            "complete-bill-accounting",
        )?;
        append_event(
            &mut tx,
            &metadata,
            command.bill_id,
            version,
            crate::contexts::sharing::public::BILL_POSITION_CHANGED_V1,
            payload,
        )
        .await?;
        let view = load_bill_tx(&mut tx, command.user_id, command.bill_id)
            .await?
            .ok_or(SharingError::NotFound)?;
        tx.commit().await.map_err(database)?;
        Ok(view)
    }

    pub(crate) async fn complete_bill_cancellation(
        &self,
        command: CompleteBillCancellation,
    ) -> Result<BillView, SharingError> {
        let mut tx = self.pool.begin().await.map_err(database)?;
        let row = lock_bill(&mut tx, command.user_id, command.bill_id).await?;
        require_bill_version(&row, command.expected_version)?;
        if row.get::<String, _>("status") != "pending_cancellation" {
            return Err(SharingError::InvalidTransition);
        }
        let revision = u32::try_from(row.get::<i32, _>("current_revision"))
            .map_err(|_| SharingError::ArithmeticOverflow)?;
        let reason = row
            .get::<Option<String>, _>("cancellation_reason")
            .ok_or(SharingError::InvalidTransition)?;
        let version = command.expected_version.0 + 1;
        sqlx::query("UPDATE sharing.bill_revisions SET accounting_reversal_journal_id=$1 WHERE bill_id=$2 AND user_id=$3 AND revision=$4").bind(command.reversal_journal_id).bind(command.bill_id.into_uuid()).bind(command.user_id.into_uuid()).bind(i32::try_from(revision).map_err(|_|SharingError::ArithmeticOverflow)?).execute(&mut *tx).await.map_err(database)?;
        sqlx::query("UPDATE sharing.bills SET status='cancelled',version=$1,updated_at=$2 WHERE id=$3 AND user_id=$4").bind(i64::try_from(version).map_err(|_|SharingError::ArithmeticOverflow)?).bind(command.occurred_at).bind(command.bill_id.into_uuid()).bind(command.user_id.into_uuid()).execute(&mut *tx).await.map_err(database)?;
        sqlx::query("UPDATE integration.process_instances SET status='cancelled',version=version+1,next_wake_at=NULL,updated_at=clock_timestamp() WHERE process_name='sharing_bill_cancellation' AND instance_key=$1")
            .bind(command.bill_id.to_string()).execute(&mut *tx).await.map_err(database)?;
        let metadata = system_metadata(
            command.user_id,
            command.correlation_id,
            command.occurred_at,
            "complete-bill-cancellation",
        )?;
        append_event(&mut tx, &metadata, command.bill_id, version, crate::contexts::sharing::public::BILL_CANCELLED_V1, json!({"bill_id":command.bill_id,"revision":revision,"bill_version":BillVersion(version),"reason":reason,"cancelled_at":command.occurred_at})).await?;
        let view = load_bill_tx(&mut tx, command.user_id, command.bill_id)
            .await?
            .ok_or(SharingError::NotFound)?;
        tx.commit().await.map_err(database)?;
        Ok(view)
    }

    pub(crate) async fn complete_settlement_accounting(
        &self,
        command: CompleteSettlementAccounting,
    ) -> Result<SettlementView, SharingError> {
        let mut tx = self.pool.begin().await.map_err(database)?;
        let row = sqlx::query("SELECT amount,currency,status,version FROM sharing.settlements WHERE id=$1 AND bill_id=$2 AND user_id=$3 FOR UPDATE")
            .bind(command.settlement_id.into_uuid()).bind(command.bill_id.into_uuid()).bind(command.user_id.into_uuid()).fetch_optional(&mut *tx).await.map_err(database)?.ok_or(SharingError::NotFound)?;
        let actual = u64::try_from(row.get::<i64, _>("version"))
            .map_err(|_| SharingError::ArithmeticOverflow)?;
        if actual != command.expected_version.0 {
            return Err(SharingError::VersionConflict {
                expected: command.expected_version.0,
                actual,
            });
        }
        if row.get::<String, _>("status") != "pending_accounting" {
            return Err(SharingError::InvalidTransition);
        }
        sqlx::query("UPDATE sharing.settlements SET status='posted',version=version+1,accounting_journal_id=$1,last_error=NULL WHERE id=$2 AND user_id=$3")
            .bind(command.journal_id).bind(command.settlement_id.into_uuid()).bind(command.user_id.into_uuid()).execute(&mut *tx).await.map_err(database)?;
        sqlx::query("UPDATE integration.process_instances SET status='posted',version=version+1,next_wake_at=NULL,updated_at=clock_timestamp() WHERE process_name='sharing_settlement' AND instance_key=$1")
            .bind(command.settlement_id.to_string()).execute(&mut *tx).await.map_err(database)?;
        let bill = lock_bill(&mut tx, command.user_id, command.bill_id).await?;
        let revision = u32::try_from(bill.get::<i32, _>("current_revision"))
            .map_err(|_| SharingError::ArithmeticOverflow)?;
        let payload = current_position(&mut tx, command.user_id, command.bill_id, revision).await?;
        let metadata = system_metadata(
            command.user_id,
            command.correlation_id,
            command.occurred_at,
            "complete-settlement-accounting",
        )?;
        append_event(
            &mut tx,
            &metadata,
            command.bill_id,
            u64::try_from(bill.get::<i64, _>("version"))
                .map_err(|_| SharingError::ArithmeticOverflow)?,
            crate::contexts::sharing::public::BILL_POSITION_CHANGED_V1,
            payload,
        )
        .await?;
        let currency = CurrencyCode::new(row.get::<String, _>("currency"))
            .map_err(|error| SharingError::Persistence(error.to_string()))?;
        let view = SettlementView {
            id: command.settlement_id,
            bill_id: command.bill_id,
            amount: row.get("amount"),
            currency,
            status: SettlementStatus::Posted,
            version: SettlementVersion(actual + 1),
            process: ProcessView {
                state: "posted".into(),
                correlation_id: command.correlation_id,
                last_error: None,
            },
        };
        tx.commit().await.map_err(database)?;
        Ok(view)
    }

    pub(crate) async fn complete_settlement_reversal(
        &self,
        command: CompleteSettlementReversal,
    ) -> Result<SettlementView, SharingError> {
        let mut tx = self.pool.begin().await.map_err(database)?;
        let row=sqlx::query("SELECT s.amount,s.currency,s.version,s.obligation_id FROM sharing.settlements s JOIN sharing.settlement_reversals r ON r.settlement_id=s.id AND r.user_id=s.user_id WHERE s.id=$1 AND s.bill_id=$2 AND s.user_id=$3 FOR UPDATE OF s,r")
            .bind(command.settlement_id.into_uuid()).bind(command.bill_id.into_uuid()).bind(command.user_id.into_uuid()).fetch_optional(&mut *tx).await.map_err(database)?.ok_or(SharingError::NotFound)?;
        sqlx::query("UPDATE sharing.settlement_reversals SET ledger_reversal_journal_id=$1 WHERE settlement_id=$2 AND user_id=$3")
            .bind(command.reversal_journal_id).bind(command.settlement_id.into_uuid()).bind(command.user_id.into_uuid()).execute(&mut *tx).await.map_err(database)?;
        sqlx::query("UPDATE sharing.obligations SET settled_amount=settled_amount-$1 WHERE id=$2 AND user_id=$3 AND settled_amount>=$1")
            .bind(row.get::<Decimal,_>("amount")).bind(row.get::<Uuid,_>("obligation_id")).bind(command.user_id.into_uuid()).execute(&mut *tx).await.map_err(database)?;
        sqlx::query("UPDATE sharing.bills SET active_settlements=active_settlements-1,version=version+1,updated_at=$1 WHERE id=$2 AND user_id=$3 AND active_settlements>0")
            .bind(command.occurred_at).bind(command.bill_id.into_uuid()).bind(command.user_id.into_uuid()).execute(&mut *tx).await.map_err(database)?;
        sqlx::query("UPDATE integration.process_instances SET status='reversed',version=version+1,next_wake_at=NULL,updated_at=clock_timestamp() WHERE process_name='sharing_settlement_reversal' AND instance_key=$1")
            .bind(command.settlement_id.to_string()).execute(&mut *tx).await.map_err(database)?;
        let bill = lock_bill(&mut tx, command.user_id, command.bill_id).await?;
        let revision = u32::try_from(bill.get::<i32, _>("current_revision"))
            .map_err(|_| SharingError::ArithmeticOverflow)?;
        let payload = current_position(&mut tx, command.user_id, command.bill_id, revision).await?;
        let metadata = system_metadata(
            command.user_id,
            command.correlation_id,
            command.occurred_at,
            "complete-settlement-reversal",
        )?;
        append_event(
            &mut tx,
            &metadata,
            command.bill_id,
            u64::try_from(bill.get::<i64, _>("version"))
                .map_err(|_| SharingError::ArithmeticOverflow)?,
            crate::contexts::sharing::public::BILL_POSITION_CHANGED_V1,
            payload,
        )
        .await?;
        let currency = CurrencyCode::new(row.get::<String, _>("currency"))
            .map_err(|error| SharingError::Persistence(error.to_string()))?;
        let version = u64::try_from(row.get::<i64, _>("version"))
            .map_err(|_| SharingError::ArithmeticOverflow)?
            + 1;
        let view = SettlementView {
            id: command.settlement_id,
            bill_id: command.bill_id,
            amount: row.get("amount"),
            currency,
            status: SettlementStatus::Reversed,
            version: SettlementVersion(version),
            process: ProcessView {
                state: "reversed".into(),
                correlation_id: command.correlation_id,
                last_error: None,
            },
        };
        tx.commit().await.map_err(database)?;
        Ok(view)
    }
}

async fn current_position(
    tx: &mut Transaction<'_, Postgres>,
    user: UserId,
    bill: BillSplitId,
    revision: u32,
) -> Result<serde_json::Value, SharingError> {
    let row=sqlx::query("SELECT r.currency,COALESCE(sum(CASE WHEN o.creditor_kind='current_user' THEN o.original_amount-o.settled_amount ELSE 0 END),0) receivable,COALESCE(sum(CASE WHEN o.debtor_kind='current_user' THEN o.original_amount-o.settled_amount ELSE 0 END),0) payable FROM sharing.bill_revisions r LEFT JOIN sharing.obligations o ON o.bill_id=r.bill_id AND o.user_id=r.user_id AND o.revision=r.revision WHERE r.bill_id=$1 AND r.user_id=$2 AND r.revision=$3 GROUP BY r.currency").bind(bill.into_uuid()).bind(user.into_uuid()).bind(i32::try_from(revision).map_err(|_|SharingError::ArithmeticOverflow)?).fetch_one(&mut **tx).await.map_err(database)?;
    Ok(
        json!({"position":{"bill_id":bill,"revision":revision,"currency":row.get::<String,_>("currency"),"receivable":row.get::<Decimal,_>("receivable").to_string(),"payable":row.get::<Decimal,_>("payable").to_string()}}),
    )
}

fn system_metadata(
    user_id: UserId,
    correlation_id: crate::shared_kernel::CorrelationId,
    occurred_at: DateTime<Utc>,
    key: &str,
) -> Result<CommandMetadata, SharingError> {
    Ok(CommandMetadata {
        user_id,
        idempotency_key: crate::shared_kernel::IdempotencyKey::new(key)
            .map_err(|error| SharingError::Persistence(error.to_string()))?,
        request_hash: [0; 32],
        correlation_id,
        occurred_at,
    })
}

async fn load_contact(
    pool: &PgPool,
    user: UserId,
    id: ContactId,
) -> Result<Option<Contact>, SharingError> {
    sqlx::query("SELECT id,user_id,display_name,note,lifecycle,version FROM sharing.contacts WHERE id=$1 AND user_id=$2").bind(id.into_uuid()).bind(user.into_uuid()).fetch_optional(pool).await.map_err(database)?.map(row_to_contact).transpose()
}
async fn load_contact_for_update(
    tx: &mut Transaction<'_, Postgres>,
    user: UserId,
    id: ContactId,
) -> Result<Option<Contact>, SharingError> {
    sqlx::query("SELECT id,user_id,display_name,note,lifecycle,version FROM sharing.contacts WHERE id=$1 AND user_id=$2 FOR UPDATE").bind(id.into_uuid()).bind(user.into_uuid()).fetch_optional(&mut **tx).await.map_err(database)?.map(row_to_contact).transpose()
}
fn row_to_contact(row: sqlx::postgres::PgRow) -> Result<Contact, SharingError> {
    let lifecycle = match row.get::<String, _>("lifecycle").as_str() {
        "active" => ContactStatus::Active,
        "archived" => ContactStatus::Archived,
        value => {
            return Err(SharingError::Persistence(format!(
                "invalid contact lifecycle {value}"
            )));
        }
    };
    Ok(Contact::rehydrate(
        ContactId::new(row.get("id")),
        UserId::new(row.get("user_id")),
        ContactName::new(row.get::<String, _>("display_name"))?,
        row.get("note"),
        lifecycle,
        ContactVersion(
            u64::try_from(row.get::<i64, _>("version"))
                .map_err(|_| SharingError::ArithmeticOverflow)?,
        ),
    ))
}
async fn persist_contact(
    tx: &mut Transaction<'_, Postgres>,
    contact: &Contact,
    expected: ContactVersion,
) -> Result<(), SharingError> {
    let affected = sqlx::query("UPDATE sharing.contacts SET display_name=$1,note=$2,lifecycle=$3,version=$4,updated_at=clock_timestamp() WHERE id=$5 AND user_id=$6 AND version=$7").bind(contact.name().as_str()).bind(contact.note()).bind(match contact.status(){ContactStatus::Active=>"active",ContactStatus::Archived=>"archived"}).bind(i64::try_from(contact.version().0).map_err(|_| SharingError::ArithmeticOverflow)?).bind(contact.id().into_uuid()).bind(contact.user_id().into_uuid()).bind(i64::try_from(expected.0).map_err(|_| SharingError::ArithmeticOverflow)?).execute(&mut **tx).await.map_err(database)?.rows_affected();
    if affected == 1 {
        Ok(())
    } else {
        Err(SharingError::VersionConflict {
            expected: expected.0,
            actual: contact.version().0,
        })
    }
}

async fn validate_contacts(
    tx: &mut Transaction<'_, Postgres>,
    user: UserId,
    draft: &BillDraft,
) -> Result<(), SharingError> {
    let mut ids = std::collections::BTreeSet::new();
    for contribution in &draft.contributions {
        if let Participant::Contact(id) = contribution.participant {
            ids.insert(id);
        }
    }
    match &draft.shares {
        ShareRequest::Exact(values) => {
            for value in values {
                if let Participant::Contact(id) = value.participant {
                    ids.insert(id);
                }
            }
        }
        ShareRequest::Equal(values) => {
            for value in values {
                if let Participant::Contact(id) = value {
                    ids.insert(*id);
                }
            }
        }
    }
    for id in ids {
        let lifecycle: Option<String> =
            sqlx::query_scalar("SELECT lifecycle FROM sharing.contacts WHERE id=$1 AND user_id=$2")
                .bind(id.into_uuid())
                .bind(user.into_uuid())
                .fetch_optional(&mut **tx)
                .await
                .map_err(database)?;
        match lifecycle.as_deref() {
            Some("active") => {}
            Some("archived") => return Err(SharingError::ContactArchived),
            _ => return Err(SharingError::NotFound),
        }
    }
    Ok(())
}

async fn insert_bill(
    tx: &mut Transaction<'_, Postgres>,
    bill: &BillSplit,
) -> Result<(), SharingError> {
    let revision = bill.current_revision();
    sqlx::query("INSERT INTO sharing.bills(id,user_id,currency,current_revision,status,active_settlements,version,created_at,updated_at) VALUES($1,$2,$3,1,'pending_accounting',0,1,$4,$4)").bind(bill.id().into_uuid()).bind(bill.user_id().into_uuid()).bind(revision.total.currency().as_str()).bind(revision.occurred_at).execute(&mut **tx).await.map_err(database)?;
    insert_revision(tx, bill.id(), bill.user_id(), revision).await
}
async fn insert_revision(
    tx: &mut Transaction<'_, Postgres>,
    bill_id: BillSplitId,
    user: UserId,
    revision: &BillRevision,
) -> Result<(), SharingError> {
    sqlx::query("INSERT INTO sharing.bill_revisions(bill_id,user_id,revision,title,occurred_at,total,currency,accounting_status,accounting_correlation_id) VALUES($1,$2,$3,$4,$5,$6,$7,'pending',$8)").bind(bill_id.into_uuid()).bind(user.into_uuid()).bind(i32::try_from(revision.number).map_err(|_| SharingError::ArithmeticOverflow)?).bind(&revision.title).bind(revision.occurred_at).bind(revision.total.amount()).bind(revision.total.currency().as_str()).bind(revision.accounting_correlation_id.into_uuid()).execute(&mut **tx).await.map_err(database)?;
    for (position, contribution) in revision.contributions.iter().enumerate() {
        let id = Uuid::new_v4();
        let (kind, contact) = participant_db(contribution.participant);
        let (evidence, account) = match &contribution.evidence {
            ContributionEvidence::External => ("external", None),
            ContributionEvidence::Manual { account_id } => ("manual", Some(account_id.into_uuid())),
            ContributionEvidence::ExistingJournals { .. } => ("existing_journals", None),
        };
        sqlx::query("INSERT INTO sharing.contributions(id,bill_id,user_id,revision,position,participant_kind,participant_contact_id,amount,currency,evidence_kind,ledger_account_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)").bind(id).bind(bill_id.into_uuid()).bind(user.into_uuid()).bind(i32::try_from(revision.number).map_err(|_| SharingError::ArithmeticOverflow)?).bind(i32::try_from(position).map_err(|_| SharingError::ArithmeticOverflow)?).bind(kind).bind(contact).bind(contribution.amount.amount()).bind(contribution.amount.currency().as_str()).bind(evidence).bind(account).execute(&mut **tx).await.map_err(database)?;
        if let ContributionEvidence::ExistingJournals { allocations } = &contribution.evidence {
            for (position, item) in allocations.iter().enumerate() {
                sqlx::query("INSERT INTO sharing.contribution_journal_allocations(contribution_id,user_id,position,ledger_journal_id,amount,currency) VALUES($1,$2,$3,$4,$5,$6)").bind(id).bind(user.into_uuid()).bind(i32::try_from(position).map_err(|_| SharingError::ArithmeticOverflow)?).bind(item.journal_id.into_uuid()).bind(item.amount.amount()).bind(item.amount.currency().as_str()).execute(&mut **tx).await.map_err(database)?;
            }
        }
    }
    for (position, share) in revision.shares.iter().enumerate() {
        let (kind, contact) = participant_db(share.participant);
        sqlx::query("INSERT INTO sharing.participant_shares(bill_id,user_id,revision,position,participant_kind,participant_contact_id,amount,currency) VALUES($1,$2,$3,$4,$5,$6,$7,$8)").bind(bill_id.into_uuid()).bind(user.into_uuid()).bind(i32::try_from(revision.number).map_err(|_| SharingError::ArithmeticOverflow)?).bind(i32::try_from(position).map_err(|_| SharingError::ArithmeticOverflow)?).bind(kind).bind(contact).bind(share.amount.amount()).bind(share.amount.currency().as_str()).execute(&mut **tx).await.map_err(database)?;
    }
    for (position, obligation) in revision.obligations.iter().enumerate() {
        let (dk, di) = participant_db(obligation.debtor);
        let (ck, ci) = participant_db(obligation.creditor);
        sqlx::query("INSERT INTO sharing.obligations(id,bill_id,user_id,revision,position,debtor_kind,debtor_contact_id,creditor_kind,creditor_contact_id,original_amount,currency) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)").bind(Uuid::new_v4()).bind(bill_id.into_uuid()).bind(user.into_uuid()).bind(i32::try_from(revision.number).map_err(|_| SharingError::ArithmeticOverflow)?).bind(i32::try_from(position).map_err(|_| SharingError::ArithmeticOverflow)?).bind(dk).bind(di).bind(ck).bind(ci).bind(obligation.amount.amount()).bind(obligation.amount.currency().as_str()).execute(&mut **tx).await.map_err(database)?;
    }
    Ok(())
}

fn participant_db(value: Participant) -> (&'static str, Option<Uuid>) {
    match value {
        Participant::CurrentUser => ("current_user", None),
        Participant::Contact(id) => ("contact", Some(id.into_uuid())),
    }
}
fn settlement_evidence_db(
    value: &SettlementEvidence,
) -> (&'static str, Option<Uuid>, Option<Uuid>) {
    match value {
        SettlementEvidence::External => ("external", None, None),
        SettlementEvidence::Manual { account_id } => ("manual", Some(account_id.into_uuid()), None),
        SettlementEvidence::ExistingJournal { journal_id } => {
            ("existing_journal", None, Some(journal_id.into_uuid()))
        }
    }
}

async fn lock_bill(
    tx: &mut Transaction<'_, Postgres>,
    user: UserId,
    id: BillSplitId,
) -> Result<sqlx::postgres::PgRow, SharingError> {
    sqlx::query("SELECT id,user_id,currency,current_revision,status,active_settlements,version,cancellation_reason FROM sharing.bills WHERE id=$1 AND user_id=$2 FOR UPDATE").bind(id.into_uuid()).bind(user.into_uuid()).fetch_optional(&mut **tx).await.map_err(database)?.ok_or(SharingError::NotFound)
}
fn require_bill_version(
    row: &sqlx::postgres::PgRow,
    expected: BillVersion,
) -> Result<(), SharingError> {
    let actual = u64::try_from(row.get::<i64, _>("version"))
        .map_err(|_| SharingError::ArithmeticOverflow)?;
    if actual == expected.0 {
        Ok(())
    } else {
        Err(SharingError::VersionConflict {
            expected: expected.0,
            actual,
        })
    }
}
async fn load_bill_tx(
    tx: &mut Transaction<'_, Postgres>,
    user: UserId,
    id: BillSplitId,
) -> Result<Option<BillView>, SharingError> {
    let row=sqlx::query("SELECT b.id,b.user_id,b.currency,b.current_revision,b.status,b.active_settlements,b.version,r.title,r.occurred_at,r.total,(SELECT jsonb_build_object('contributions',COALESCE(jsonb_agg(x.value ORDER BY x.position) FILTER(WHERE x.kind='contribution'),'[]'::jsonb),'shares',COALESCE(jsonb_agg(x.value ORDER BY x.position) FILTER(WHERE x.kind='share'),'[]'::jsonb),'obligations',COALESCE(jsonb_agg(x.value ORDER BY x.position) FILTER(WHERE x.kind='obligation'),'[]'::jsonb)) FROM (SELECT 'contribution' kind,c.position,jsonb_build_object('participant_kind',c.participant_kind,'contact_id',c.participant_contact_id,'amount',c.amount::text,'evidence',c.evidence_kind) value FROM sharing.contributions c WHERE c.bill_id=b.id AND c.user_id=b.user_id AND c.revision=b.current_revision UNION ALL SELECT 'share',s.position,jsonb_build_object('participant_kind',s.participant_kind,'contact_id',s.participant_contact_id,'amount',s.amount::text) FROM sharing.participant_shares s WHERE s.bill_id=b.id AND s.user_id=b.user_id AND s.revision=b.current_revision UNION ALL SELECT 'obligation',o.position,jsonb_build_object('id',o.id,'debtor_kind',o.debtor_kind,'debtor_contact_id',o.debtor_contact_id,'creditor_kind',o.creditor_kind,'creditor_contact_id',o.creditor_contact_id,'amount',o.original_amount::text,'settled_amount',o.settled_amount::text) FROM sharing.obligations o WHERE o.bill_id=b.id AND o.user_id=b.user_id AND o.revision=b.current_revision) x) allocations FROM sharing.bills b JOIN sharing.bill_revisions r ON r.bill_id=b.id AND r.user_id=b.user_id AND r.revision=b.current_revision WHERE b.id=$1 AND b.user_id=$2").bind(id.into_uuid()).bind(user.into_uuid()).fetch_optional(&mut **tx).await.map_err(database)?;
    row.map(row_to_bill_view).transpose()
}
fn row_to_bill_view(row: sqlx::postgres::PgRow) -> Result<BillView, SharingError> {
    let status = match row.get::<String, _>("status").as_str() {
        "pending_accounting" => BillStatus::PendingAccounting,
        "active" => BillStatus::Active,
        "failed" => BillStatus::Failed,
        "pending_cancellation" => BillStatus::PendingCancellation,
        "cancelled" => BillStatus::Cancelled,
        value => {
            return Err(SharingError::Persistence(format!(
                "invalid bill status {value}"
            )));
        }
    };
    Ok(BillView {
        id: BillSplitId::new(row.get("id")),
        user_id: UserId::new(row.get("user_id")),
        title: row.get("title"),
        occurred_at: row.get("occurred_at"),
        total: row.get("total"),
        currency: CurrencyCode::new(row.get::<String, _>("currency"))
            .map_err(|error| SharingError::Persistence(error.to_string()))?,
        current_revision: u32::try_from(row.get::<i32, _>("current_revision"))
            .map_err(|_| SharingError::ArithmeticOverflow)?,
        status,
        version: BillVersion(
            u64::try_from(row.get::<i64, _>("version"))
                .map_err(|_| SharingError::ArithmeticOverflow)?,
        ),
        active_settlements: u32::try_from(row.get::<i32, _>("active_settlements"))
            .map_err(|_| SharingError::ArithmeticOverflow)?,
        allocations: row
            .get::<Option<serde_json::Value>, _>("allocations")
            .unwrap_or_else(|| json!({})),
    })
}
fn bill_view_from_domain(bill: &BillSplit) -> Result<BillView, SharingError> {
    let revision = bill.current_revision();
    Ok(BillView {
        id: bill.id(),
        user_id: bill.user_id(),
        title: revision.title.clone(),
        occurred_at: revision.occurred_at,
        total: revision.total.amount(),
        currency: revision.total.currency().clone(),
        current_revision: revision.number,
        status: bill.status(),
        version: bill.version(),
        active_settlements: bill.active_settlements(),
        allocations: json!({"contributions":revision.contributions,"shares":revision.shares,"obligations":revision.obligations}),
    })
}

async fn replay<T: DeserializeOwned>(
    tx: &mut Transaction<'_, Postgres>,
    user: UserId,
    scope: &str,
    metadata: &CommandMetadata,
) -> Result<Option<T>, SharingError> {
    let row=sqlx::query("SELECT canonical_request_hash,durable_result FROM sharing.command_receipts WHERE user_id=$1 AND command_scope=$2 AND idempotency_key=$3 FOR UPDATE").bind(user.into_uuid()).bind(scope).bind(metadata.idempotency_key.as_str()).fetch_optional(&mut **tx).await.map_err(database)?;
    let Some(row) = row else { return Ok(None) };
    if row.get::<Vec<u8>, _>("canonical_request_hash") != metadata.request_hash {
        return Err(SharingError::IdempotencyConflict);
    };
    serde_json::from_value(row.get("durable_result"))
        .map(Some)
        .map_err(|error| SharingError::Persistence(error.to_string()))
}
async fn save_receipt<T: Serialize>(
    tx: &mut Transaction<'_, Postgres>,
    user: UserId,
    scope: &str,
    metadata: &CommandMetadata,
    status: i16,
    result: &T,
) -> Result<(), SharingError> {
    sqlx::query("INSERT INTO sharing.command_receipts(user_id,command_scope,idempotency_key,canonical_request_hash,result_status,durable_result) VALUES($1,$2,$3,$4,$5,$6)").bind(user.into_uuid()).bind(scope).bind(metadata.idempotency_key.as_str()).bind(metadata.request_hash.as_slice()).bind(status).bind(serde_json::to_value(result).map_err(|error|SharingError::Persistence(error.to_string()))?).execute(&mut **tx).await.map_err(database)?;
    Ok(())
}
async fn audit(
    tx: &mut Transaction<'_, Postgres>,
    user: UserId,
    kind: &str,
    id: Uuid,
    version: u64,
    action: &str,
    correlation: Uuid,
) -> Result<(), SharingError> {
    sqlx::query("INSERT INTO sharing.audit_log(user_id,aggregate_type,aggregate_id,aggregate_version,action,correlation_id) VALUES($1,$2,$3,$4,$5,$6)").bind(user.into_uuid()).bind(kind).bind(id).bind(i64::try_from(version).map_err(|_|SharingError::ArithmeticOverflow)?).bind(action).bind(correlation).execute(&mut **tx).await.map_err(database)?;
    Ok(())
}
async fn create_process(
    tx: &mut Transaction<'_, Postgres>,
    name: &str,
    key: &str,
    correlation: crate::shared_kernel::CorrelationId,
    state: serde_json::Value,
) -> Result<(), SharingError> {
    sqlx::query("INSERT INTO integration.process_instances(process_name,instance_key,state,status,version) VALUES($1,$2,$3,'pending',1) ON CONFLICT(process_name,instance_key) DO NOTHING").bind(name).bind(key).bind(json!({"correlation_id":correlation,"workflow":state})).execute(&mut **tx).await.map_err(database)?;
    Ok(())
}
async fn append_event(
    tx: &mut Transaction<'_, Postgres>,
    metadata: &CommandMetadata,
    bill_id: BillSplitId,
    version: u64,
    event_type: &str,
    payload: serde_json::Value,
) -> Result<(), SharingError> {
    sqlx::query("INSERT INTO integration.outbox_messages(message_id,event_id,message_schema_version,context_name,aggregate_id,aggregate_version,event_type,user_id,occurred_at,correlation_id,payload) VALUES($1,$2,1,'sharing',$3,$4,$5,$6,$7,$8,$9)").bind(Uuid::new_v4()).bind(Uuid::new_v4()).bind(bill_id.to_string()).bind(i64::try_from(version).map_err(|_|SharingError::ArithmeticOverflow)?).bind(event_type).bind(metadata.user_id.into_uuid()).bind(metadata.occurred_at).bind(metadata.correlation_id.into_uuid()).bind(payload).execute(&mut **tx).await.map_err(database)?;
    Ok(())
}
fn database(error: sqlx::Error) -> SharingError {
    SharingError::Persistence(error.to_string())
}
