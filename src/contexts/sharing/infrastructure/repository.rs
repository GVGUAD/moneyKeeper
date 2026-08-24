//! PostgreSQL aggregate repositories and atomic Sharing unit of work.

use async_trait::async_trait;
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

async fn verify_workflow_claim(
    tx: &mut Transaction<'_, Postgres>,
    claim: &WorkflowClaim,
) -> Result<(), SharingError> {
    let valid: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM integration.process_leases WHERE process_name=$1 AND instance_key=$2 AND holder=$3 AND fencing_token=$4 AND expires_at>clock_timestamp() FOR UPDATE)")
        .bind(&claim.process_name).bind(&claim.instance_key).bind(&claim.holder).bind(claim.fencing_token).fetch_one(&mut **tx).await.map_err(database)?;
    if valid {
        Ok(())
    } else {
        Err(SharingError::Persistence(
            "workflow lease was fenced".into(),
        ))
    }
}

async fn finish_failed_process(
    tx: &mut Transaction<'_, Postgres>,
    claim: &WorkflowClaim,
    error: &str,
) -> Result<(), SharingError> {
    sqlx::query("UPDATE integration.process_instances SET status='failed',state=jsonb_set(state,'{last_error}',to_jsonb($3::text),true),next_wake_at=NULL,version=version+1,updated_at=clock_timestamp() WHERE process_name=$1 AND instance_key=$2")
        .bind(&claim.process_name).bind(&claim.instance_key).bind(truncate_error(error)).execute(&mut **tx).await.map_err(database)?;
    Ok(())
}

async fn load_process_correlation(
    tx: &mut Transaction<'_, Postgres>,
    claim: &WorkflowClaim,
) -> Result<crate::shared_kernel::CorrelationId, SharingError> {
    let value: serde_json::Value = sqlx::query_scalar(
        "SELECT state FROM integration.process_instances WHERE process_name=$1 AND instance_key=$2",
    )
    .bind(&claim.process_name)
    .bind(&claim.instance_key)
    .fetch_one(&mut **tx)
    .await
    .map_err(database)?;
    serde_json::from_value(
        value
            .get("correlation_id")
            .cloned()
            .ok_or_else(|| SharingError::Persistence("workflow correlation is missing".into()))?,
    )
    .map_err(|error| SharingError::Persistence(error.to_string()))
}

fn truncate_error(value: &str) -> String {
    value.chars().take(1000).collect()
}

fn workflow_settlement_id(value: &serde_json::Value) -> Result<SettlementId, SharingError> {
    serde_json::from_value(
        value
            .get("settlement_id")
            .cloned()
            .ok_or_else(|| SharingError::Persistence("settlement id is missing".into()))?,
    )
    .map_err(|error| SharingError::Persistence(error.to_string()))
}

async fn latest_posted_journals_before(
    tx: &mut Transaction<'_, Postgres>,
    user: UserId,
    bill: BillSplitId,
    before_revision: u32,
) -> Result<Vec<Uuid>, SharingError> {
    sqlx::query_scalar("WITH latest AS (SELECT max(revision) revision FROM sharing.bill_revisions WHERE bill_id=$1 AND user_id=$2 AND revision<$3 AND accounting_status='posted') SELECT j.ledger_journal_id FROM sharing.bill_revision_accounting_journals j JOIN latest l ON l.revision=j.revision WHERE j.bill_id=$1 AND j.user_id=$2 AND j.ledger_reversal_journal_id IS NULL ORDER BY j.position")
        .bind(bill.into_uuid()).bind(user.into_uuid()).bind(i32::try_from(before_revision).map_err(|_|SharingError::ArithmeticOverflow)?).fetch_all(&mut **tx).await.map_err(database)
}

async fn load_bill_domain_tx(
    tx: &mut Transaction<'_, Postgres>,
    bill_id: BillSplitId,
) -> Result<BillSplit, SharingError> {
    let row=sqlx::query("SELECT b.user_id,b.status,b.version,b.active_settlements,b.cancellation_reason,b.current_revision,r.title,r.occurred_at,r.total,r.currency,r.accounting_status,r.accounting_correlation_id FROM sharing.bills b JOIN sharing.bill_revisions r ON r.bill_id=b.id AND r.user_id=b.user_id AND r.revision=b.current_revision WHERE b.id=$1 FOR UPDATE OF b")
        .bind(bill_id.into_uuid()).fetch_optional(&mut **tx).await.map_err(database)?.ok_or(SharingError::NotFound)?;
    let user = UserId::new(row.get("user_id"));
    let revision_i32: i32 = row.get("current_revision");
    let revision_number =
        u32::try_from(revision_i32).map_err(|_| SharingError::ArithmeticOverflow)?;
    let currency = CurrencyCode::new(row.get::<String, _>("currency"))
        .map_err(|error| SharingError::Persistence(error.to_string()))?;
    let total = Money::new(row.get("total"), currency.clone(), Money::DATABASE_SCALE)?;

    let contribution_rows=sqlx::query("SELECT id,participant_kind,participant_contact_id,amount,evidence_kind,ledger_account_id FROM sharing.contributions WHERE bill_id=$1 AND user_id=$2 AND revision=$3 ORDER BY position")
        .bind(bill_id.into_uuid()).bind(user.into_uuid()).bind(revision_i32).fetch_all(&mut **tx).await.map_err(database)?;
    let mut contributions = Vec::with_capacity(contribution_rows.len());
    for contribution in contribution_rows {
        let contribution_id: Uuid = contribution.get("id");
        let participant = participant_from_db(
            contribution.get::<String, _>("participant_kind").as_str(),
            contribution.get("participant_contact_id"),
        )?;
        let amount = Money::new(
            contribution.get("amount"),
            currency.clone(),
            Money::DATABASE_SCALE,
        )?;
        let evidence = match contribution.get::<String, _>("evidence_kind").as_str() {
            "external" => ContributionEvidence::External,
            "manual" => ContributionEvidence::Manual {
                account_id: LedgerAccountReference::new(
                    contribution.get::<Uuid, _>("ledger_account_id"),
                ),
            },
            "existing_journals" => {
                let rows=sqlx::query("SELECT ledger_journal_id,amount,currency FROM sharing.contribution_journal_allocations WHERE contribution_id=$1 AND user_id=$2 ORDER BY position")
                    .bind(contribution_id).bind(user.into_uuid()).fetch_all(&mut **tx).await.map_err(database)?;
                let allocations = rows
                    .into_iter()
                    .map(|value| {
                        let item_currency =
                            CurrencyCode::new(value.get::<String, _>("currency"))
                                .map_err(|error| SharingError::Persistence(error.to_string()))?;
                        Ok(JournalAllocation {
                            journal_id: LedgerJournalReference::new(value.get("ledger_journal_id")),
                            amount: Money::new(
                                value.get("amount"),
                                item_currency,
                                Money::DATABASE_SCALE,
                            )?,
                        })
                    })
                    .collect::<Result<Vec<_>, SharingError>>()?;
                ContributionEvidence::ExistingJournals { allocations }
            }
            value => {
                return Err(SharingError::Persistence(format!(
                    "invalid contribution evidence {value}"
                )));
            }
        };
        contributions.push(Contribution::new(participant, amount, evidence)?);
    }
    let share_rows=sqlx::query("SELECT participant_kind,participant_contact_id,amount FROM sharing.participant_shares WHERE bill_id=$1 AND user_id=$2 AND revision=$3 ORDER BY position")
        .bind(bill_id.into_uuid()).bind(user.into_uuid()).bind(revision_i32).fetch_all(&mut **tx).await.map_err(database)?;
    let shares = share_rows
        .into_iter()
        .map(|value| {
            Ok(ParticipantShare {
                participant: participant_from_db(
                    value.get::<String, _>("participant_kind").as_str(),
                    value.get("participant_contact_id"),
                )?,
                amount: Money::new(value.get("amount"), currency.clone(), Money::DATABASE_SCALE)?,
            })
        })
        .collect::<Result<Vec<_>, SharingError>>()?;
    let obligation_rows=sqlx::query("SELECT debtor_kind,debtor_contact_id,creditor_kind,creditor_contact_id,original_amount FROM sharing.obligations WHERE bill_id=$1 AND user_id=$2 AND revision=$3 ORDER BY position")
        .bind(bill_id.into_uuid()).bind(user.into_uuid()).bind(revision_i32).fetch_all(&mut **tx).await.map_err(database)?;
    let obligations = obligation_rows
        .into_iter()
        .map(|value| {
            Ok(Obligation {
                debtor: participant_from_db(
                    value.get::<String, _>("debtor_kind").as_str(),
                    value.get("debtor_contact_id"),
                )?,
                creditor: participant_from_db(
                    value.get::<String, _>("creditor_kind").as_str(),
                    value.get("creditor_contact_id"),
                )?,
                amount: Money::new(
                    value.get("original_amount"),
                    currency.clone(),
                    Money::DATABASE_SCALE,
                )?,
            })
        })
        .collect::<Result<Vec<_>, SharingError>>()?;
    let mut revision = BillRevision::new(
        revision_number,
        row.get::<String, _>("title"),
        row.get("occurred_at"),
        total,
        contributions,
        shares,
        obligations,
        crate::shared_kernel::CorrelationId::new(row.get("accounting_correlation_id")),
    )?;
    revision.accounting_status = match row.get::<String, _>("accounting_status").as_str() {
        "pending" => AccountingStatus::Pending,
        "posted" => AccountingStatus::Posted,
        "failed" => AccountingStatus::Failed,
        value => {
            return Err(SharingError::Persistence(format!(
                "invalid accounting status {value}"
            )));
        }
    };
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
    BillSplit::rehydrate(
        bill_id,
        user,
        vec![revision],
        status,
        BillVersion(
            u64::try_from(row.get::<i64, _>("version"))
                .map_err(|_| SharingError::ArithmeticOverflow)?,
        ),
        u32::try_from(row.get::<i32, _>("active_settlements"))
            .map_err(|_| SharingError::ArithmeticOverflow)?,
        row.get("cancellation_reason"),
    )
}

async fn load_settlement_domain_tx(
    tx: &mut Transaction<'_, Postgres>,
    settlement_id: SettlementId,
) -> Result<Settlement, SharingError> {
    let row=sqlx::query("SELECT s.bill_id,s.user_id,s.amount,s.currency,s.evidence_kind,s.ledger_account_id,s.ledger_journal_id,s.status,s.version,s.occurred_at,o.debtor_kind,o.debtor_contact_id,o.creditor_kind,o.creditor_contact_id,r.reason reversal_reason FROM sharing.settlements s JOIN sharing.obligations o ON o.id=s.obligation_id AND o.user_id=s.user_id LEFT JOIN sharing.settlement_reversals r ON r.settlement_id=s.id AND r.user_id=s.user_id WHERE s.id=$1 FOR UPDATE OF s")
        .bind(settlement_id.into_uuid()).fetch_optional(&mut **tx).await.map_err(database)?.ok_or(SharingError::NotFound)?;
    let user = UserId::new(row.get("user_id"));
    let currency = CurrencyCode::new(row.get::<String, _>("currency"))
        .map_err(|error| SharingError::Persistence(error.to_string()))?;
    let evidence = match row.get::<String, _>("evidence_kind").as_str() {
        "external" => SettlementEvidence::External,
        "manual" => SettlementEvidence::Manual {
            account_id: LedgerAccountReference::new(row.get::<Uuid, _>("ledger_account_id")),
        },
        "existing_journal" => SettlementEvidence::ExistingJournal {
            journal_id: LedgerJournalReference::new(row.get::<Uuid, _>("ledger_journal_id")),
        },
        value => {
            return Err(SharingError::Persistence(format!(
                "invalid settlement evidence {value}"
            )));
        }
    };
    let reversal_reason: Option<String> = row.get("reversal_reason");
    let status = if reversal_reason.is_some() {
        SettlementStatus::Reversed
    } else {
        match row.get::<String, _>("status").as_str() {
            "pending_accounting" => SettlementStatus::PendingAccounting,
            "posted" => SettlementStatus::Posted,
            "failed" => SettlementStatus::Failed,
            value => {
                return Err(SharingError::Persistence(format!(
                    "invalid settlement status {value}"
                )));
            }
        }
    };
    Ok(Settlement::rehydrate(
        settlement_id,
        BillSplitId::new(row.get("bill_id")),
        user,
        participant_from_db(
            row.get::<String, _>("debtor_kind").as_str(),
            row.get("debtor_contact_id"),
        )?,
        participant_from_db(
            row.get::<String, _>("creditor_kind").as_str(),
            row.get("creditor_contact_id"),
        )?,
        Money::new(row.get("amount"), currency, Money::DATABASE_SCALE)?,
        evidence,
        status,
        SettlementVersion(
            u64::try_from(row.get::<i64, _>("version"))
                .map_err(|_| SharingError::ArithmeticOverflow)?,
        ),
        row.get("occurred_at"),
        reversal_reason,
    ))
}

fn participant_from_db(kind: &str, contact: Option<Uuid>) -> Result<Participant, SharingError> {
    match (kind, contact) {
        ("current_user", None) => Ok(Participant::CurrentUser),
        ("contact", Some(id)) => Ok(Participant::Contact(ContactId::new(id))),
        _ => Err(SharingError::Persistence(
            "invalid stored participant".into(),
        )),
    }
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
        let view = load_bill_tx(&mut tx, command.metadata.user_id, bill.id())
            .await?
            .ok_or(SharingError::NotFound)?;
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
            obligation.get::<Decimal, _>("remaining").normalize(),
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
            debtor: Some(settlement.debtor()),
            creditor: Some(settlement.creditor()),
            amount: settlement.amount().amount(),
            currency,
            evidence: Some(settlement.evidence().clone()),
            occurred_at: Some(settlement.occurred_at()),
            accounting_journal_id: None,
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

    pub(crate) async fn settlements(
        &self,
        user: UserId,
        bill: BillSplitId,
    ) -> Result<Vec<SettlementView>, SharingError> {
        let rows=sqlx::query("SELECT s.id,s.bill_id,s.amount,s.currency,s.evidence_kind,s.ledger_account_id,s.ledger_journal_id,s.status,s.version,s.occurred_at,s.accounting_correlation_id,s.accounting_journal_id,s.last_error,o.debtor_kind,o.debtor_contact_id,o.creditor_kind,o.creditor_contact_id,r.settlement_id IS NOT NULL reversed,p.status process_status,p.state process_state FROM sharing.settlements s JOIN sharing.obligations o ON o.id=s.obligation_id AND o.user_id=s.user_id LEFT JOIN sharing.settlement_reversals r ON r.settlement_id=s.id AND r.user_id=s.user_id LEFT JOIN integration.process_instances p ON p.process_name=CASE WHEN r.settlement_id IS NULL THEN 'sharing_settlement' ELSE 'sharing_settlement_reversal' END AND p.instance_key=s.id::text WHERE s.bill_id=$1 AND s.user_id=$2 ORDER BY s.recorded_at,s.id")
            .bind(bill.into_uuid()).bind(user.into_uuid()).fetch_all(&self.pool).await.map_err(database)?;
        rows.into_iter()
            .map(|row| {
                let currency = CurrencyCode::new(row.get::<String, _>("currency"))
                    .map_err(|error| SharingError::Persistence(error.to_string()))?;
                let evidence = match row.get::<String, _>("evidence_kind").as_str() {
                    "external" => SettlementEvidence::External,
                    "manual" => SettlementEvidence::Manual {
                        account_id: LedgerAccountReference::new(
                            row.get::<Uuid, _>("ledger_account_id"),
                        ),
                    },
                    "existing_journal" => SettlementEvidence::ExistingJournal {
                        journal_id: LedgerJournalReference::new(
                            row.get::<Uuid, _>("ledger_journal_id"),
                        ),
                    },
                    value => {
                        return Err(SharingError::Persistence(format!(
                            "invalid settlement evidence {value}"
                        )));
                    }
                };
                let reversed: bool = row.get("reversed");
                let status = if reversed {
                    SettlementStatus::Reversed
                } else {
                    match row.get::<String, _>("status").as_str() {
                        "pending_accounting" => SettlementStatus::PendingAccounting,
                        "posted" => SettlementStatus::Posted,
                        "failed" => SettlementStatus::Failed,
                        value => {
                            return Err(SharingError::Persistence(format!(
                                "invalid settlement status {value}"
                            )));
                        }
                    }
                };
                let process_state: Option<serde_json::Value> = row.get("process_state");
                let correlation_id = process_state
                    .as_ref()
                    .and_then(|value| value.get("correlation_id"))
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()
                    .map_err(|error| SharingError::Persistence(error.to_string()))?
                    .unwrap_or_else(|| {
                        crate::shared_kernel::CorrelationId::new(
                            row.get("accounting_correlation_id"),
                        )
                    });
                let last_error = process_state
                    .as_ref()
                    .and_then(|value| value.get("last_error"))
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned)
                    .or_else(|| row.get("last_error"));
                Ok(SettlementView {
                    id: SettlementId::new(row.get("id")),
                    bill_id: BillSplitId::new(row.get("bill_id")),
                    debtor: Some(participant_from_db(
                        row.get::<String, _>("debtor_kind").as_str(),
                        row.get("debtor_contact_id"),
                    )?),
                    creditor: Some(participant_from_db(
                        row.get::<String, _>("creditor_kind").as_str(),
                        row.get("creditor_contact_id"),
                    )?),
                    amount: row.get("amount"),
                    currency,
                    evidence: Some(evidence),
                    occurred_at: Some(row.get("occurred_at")),
                    accounting_journal_id: row.get("accounting_journal_id"),
                    status,
                    version: SettlementVersion(
                        u64::try_from(row.get::<i64, _>("version"))
                            .map_err(|_| SharingError::ArithmeticOverflow)?
                            + u64::from(reversed),
                    ),
                    process: ProcessView {
                        state: row
                            .get::<Option<String>, _>("process_status")
                            .unwrap_or_else(|| "unknown".into()),
                        correlation_id,
                        last_error,
                    },
                })
            })
            .collect()
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
        if status != "posted" {
            return Err(if status == "pending_accounting" {
                SharingError::AccountingPending
            } else {
                SharingError::InvalidTransition
            });
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
            debtor: None,
            creditor: None,
            amount: row.get("amount"),
            currency,
            evidence: None,
            occurred_at: None,
            accounting_journal_id: None,
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
        if let Some(claim) = &command.claim {
            verify_workflow_claim(&mut tx, claim).await?;
        }
        let row = lock_bill(&mut tx, command.user_id, command.bill_id).await?;
        require_bill_version(&row, command.expected_version)?;
        let current_revision = u32::try_from(row.get::<i32, _>("current_revision"))
            .map_err(|_| SharingError::ArithmeticOverflow)?;
        if row.get::<String, _>("status") != "pending_accounting"
            || current_revision != command.revision
        {
            return Err(SharingError::InvalidTransition);
        }
        for reversal in &command.reversed_journals {
            sqlx::query("UPDATE sharing.bill_revision_accounting_journals SET ledger_reversal_journal_id=$1 WHERE user_id=$2 AND ledger_journal_id=$3 AND ledger_reversal_journal_id IS NULL")
                .bind(reversal.reversal_journal_id).bind(command.user_id.into_uuid()).bind(reversal.original_journal_id).execute(&mut *tx).await.map_err(database)?;
        }
        for (position, journal_id) in command.journal_ids.iter().enumerate() {
            sqlx::query("INSERT INTO sharing.bill_revision_accounting_journals(bill_id,user_id,revision,position,ledger_journal_id) VALUES($1,$2,$3,$4,$5) ON CONFLICT(bill_id,user_id,revision,position) DO UPDATE SET ledger_journal_id=EXCLUDED.ledger_journal_id")
                .bind(command.bill_id.into_uuid()).bind(command.user_id.into_uuid()).bind(i32::try_from(command.revision).map_err(|_|SharingError::ArithmeticOverflow)?).bind(i32::try_from(position).map_err(|_|SharingError::ArithmeticOverflow)?).bind(journal_id).execute(&mut *tx).await.map_err(database)?;
        }
        sqlx::query("UPDATE sharing.bill_revisions SET accounting_status='posted',accounting_journal_id=$1,last_error=NULL WHERE bill_id=$2 AND user_id=$3 AND revision=$4")
            .bind(command.journal_ids.first().copied()).bind(command.bill_id.into_uuid()).bind(command.user_id.into_uuid()).bind(i32::try_from(command.revision).map_err(|_|SharingError::ArithmeticOverflow)?).execute(&mut *tx).await.map_err(database)?;
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
        if let Some(claim) = &command.claim {
            verify_workflow_claim(&mut tx, claim).await?;
        }
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
        for reversal in &command.reversed_journals {
            sqlx::query("UPDATE sharing.bill_revision_accounting_journals SET ledger_reversal_journal_id=$1 WHERE user_id=$2 AND ledger_journal_id=$3 AND ledger_reversal_journal_id IS NULL")
                .bind(reversal.reversal_journal_id).bind(command.user_id.into_uuid()).bind(reversal.original_journal_id).execute(&mut *tx).await.map_err(database)?;
        }
        sqlx::query("UPDATE sharing.bill_revisions SET accounting_reversal_journal_id=$1 WHERE bill_id=$2 AND user_id=$3 AND revision=$4").bind(command.reversed_journals.first().map(|value| value.reversal_journal_id)).bind(command.bill_id.into_uuid()).bind(command.user_id.into_uuid()).bind(i32::try_from(revision).map_err(|_|SharingError::ArithmeticOverflow)?).execute(&mut *tx).await.map_err(database)?;
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
        if let Some(claim) = &command.claim {
            verify_workflow_claim(&mut tx, claim).await?;
        }
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
            debtor: None,
            creditor: None,
            amount: row.get("amount"),
            currency,
            evidence: None,
            occurred_at: None,
            accounting_journal_id: command.journal_id,
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
        if let Some(claim) = &command.claim {
            verify_workflow_claim(&mut tx, claim).await?;
        }
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
            debtor: None,
            creditor: None,
            amount: row.get("amount"),
            currency,
            evidence: None,
            occurred_at: None,
            accounting_journal_id: command.reversal_journal_id,
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

    pub(crate) async fn claim_next_work(
        &self,
        holder: &str,
    ) -> Result<Option<SharingWorkflowWork>, SharingError> {
        if holder.is_empty() || holder.len() > 200 || holder.trim() != holder {
            return Err(SharingError::Persistence("invalid workflow holder".into()));
        }
        let mut tx = self.pool.begin().await.map_err(database)?;
        let row = sqlx::query(
            "SELECT process_name,instance_key,state FROM integration.process_instances WHERE process_name IN ('sharing_bill_accounting','sharing_bill_cancellation','sharing_settlement','sharing_settlement_reversal') AND ((status IN ('pending','retrying') AND (next_wake_at IS NULL OR next_wake_at<=clock_timestamp())) OR (status='processing' AND next_wake_at<=clock_timestamp())) ORDER BY COALESCE(next_wake_at,created_at),created_at,process_name,instance_key FOR UPDATE SKIP LOCKED LIMIT 1",
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(database)?;
        let Some(row) = row else {
            tx.rollback().await.ok();
            return Ok(None);
        };
        let process_name: String = row.get("process_name");
        let instance_key: String = row.get("instance_key");
        let state: serde_json::Value = row.get("state");
        let previous_attempt = state
            .get("attempt")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let attempt = u32::try_from(previous_attempt.saturating_add(1))
            .map_err(|_| SharingError::ArithmeticOverflow)?;
        let lease = sqlx::query("INSERT INTO integration.process_leases(process_name,instance_key,holder,expires_at,fencing_token) VALUES($1,$2,$3,clock_timestamp()+interval '30 seconds',1) ON CONFLICT(process_name,instance_key) DO UPDATE SET holder=EXCLUDED.holder,expires_at=EXCLUDED.expires_at,fencing_token=integration.process_leases.fencing_token+1 WHERE integration.process_leases.expires_at<=clock_timestamp() OR integration.process_leases.holder=EXCLUDED.holder RETURNING fencing_token,expires_at")
            .bind(&process_name).bind(&instance_key).bind(holder).fetch_optional(&mut *tx).await.map_err(database)?;
        let Some(lease) = lease else {
            tx.rollback().await.ok();
            return Ok(None);
        };
        let fencing_token: i64 = lease.get("fencing_token");
        let expires_at: DateTime<Utc> = lease.get("expires_at");
        sqlx::query("UPDATE integration.process_instances SET status='processing',state=jsonb_set(jsonb_set(state,'{attempt}',to_jsonb($3::bigint),true),'{last_error}','null'::jsonb,true),next_wake_at=$4,version=version+1,updated_at=clock_timestamp() WHERE process_name=$1 AND instance_key=$2")
            .bind(&process_name).bind(&instance_key).bind(i64::from(attempt)).bind(expires_at).execute(&mut *tx).await.map_err(database)?;
        let claim = WorkflowClaim {
            process_name: process_name.clone(),
            instance_key,
            holder: holder.to_owned(),
            fencing_token,
            attempt,
            correlation_id: serde_json::from_value(
                state.get("correlation_id").cloned().ok_or_else(|| {
                    SharingError::Persistence("workflow correlation is missing".into())
                })?,
            )
            .map_err(|error| SharingError::Persistence(error.to_string()))?,
        };
        let workflow = state
            .get("workflow")
            .ok_or_else(|| SharingError::Persistence("workflow state is missing".into()))?;
        let work = match process_name.as_str() {
            "sharing_bill_accounting" => {
                let bill_id: BillSplitId = serde_json::from_value(
                    workflow
                        .get("bill_id")
                        .cloned()
                        .ok_or_else(|| SharingError::Persistence("bill id is missing".into()))?,
                )
                .map_err(|error| SharingError::Persistence(error.to_string()))?;
                let bill = load_bill_domain_tx(&mut tx, bill_id).await?;
                let journals_to_reverse = latest_posted_journals_before(
                    &mut tx,
                    bill.user_id(),
                    bill.id(),
                    bill.current_revision().number,
                )
                .await?;
                SharingWorkflowWork::BillAccounting {
                    claim,
                    bill,
                    journals_to_reverse,
                }
            }
            "sharing_bill_cancellation" => {
                let bill_id: BillSplitId = serde_json::from_value(
                    workflow
                        .get("bill_id")
                        .cloned()
                        .ok_or_else(|| SharingError::Persistence("bill id is missing".into()))?,
                )
                .map_err(|error| SharingError::Persistence(error.to_string()))?;
                let bill = load_bill_domain_tx(&mut tx, bill_id).await?;
                let journals_to_reverse = latest_posted_journals_before(
                    &mut tx,
                    bill.user_id(),
                    bill.id(),
                    bill.current_revision().number.saturating_add(1),
                )
                .await?;
                SharingWorkflowWork::BillCancellation {
                    claim,
                    bill,
                    journals_to_reverse,
                }
            }
            "sharing_settlement" => SharingWorkflowWork::SettlementAccounting {
                settlement: load_settlement_domain_tx(&mut tx, workflow_settlement_id(workflow)?)
                    .await?,
                claim,
            },
            "sharing_settlement_reversal" => {
                let settlement_id = workflow_settlement_id(workflow)?;
                let settlement = load_settlement_domain_tx(&mut tx, settlement_id).await?;
                let row = sqlx::query("SELECT s.accounting_journal_id,r.reason FROM sharing.settlements s JOIN sharing.settlement_reversals r ON r.settlement_id=s.id AND r.user_id=s.user_id WHERE s.id=$1 AND s.user_id=$2")
                    .bind(settlement_id.into_uuid()).bind(settlement.user_id().into_uuid()).fetch_one(&mut *tx).await.map_err(database)?;
                SharingWorkflowWork::SettlementReversal {
                    claim,
                    settlement,
                    accounting_journal_id: row.get("accounting_journal_id"),
                    reason: row.get("reason"),
                }
            }
            _ => return Err(SharingError::Persistence("unknown workflow".into())),
        };
        tx.commit().await.map_err(database)?;
        Ok(Some(work))
    }

    pub(crate) async fn retry_work(
        &self,
        command: RetrySharingWorkflow,
    ) -> Result<(), SharingError> {
        let mut tx = self.pool.begin().await.map_err(database)?;
        verify_workflow_claim(&mut tx, &command.claim).await?;
        sqlx::query("UPDATE integration.process_instances SET status='retrying',state=jsonb_set(state,'{last_error}',to_jsonb($3::text),true),next_wake_at=$4,version=version+1,updated_at=clock_timestamp() WHERE process_name=$1 AND instance_key=$2")
            .bind(&command.claim.process_name).bind(&command.claim.instance_key).bind(truncate_error(&command.error)).bind(command.retry_at).execute(&mut *tx).await.map_err(database)?;
        tx.commit().await.map_err(database)
    }

    pub(crate) async fn fail_bill_accounting(
        &self,
        command: FailBillAccounting,
    ) -> Result<BillView, SharingError> {
        let mut tx = self.pool.begin().await.map_err(database)?;
        verify_workflow_claim(&mut tx, &command.claim).await?;
        let bill = lock_bill(&mut tx, command.user_id, command.bill_id).await?;
        require_bill_version(&bill, command.expected_version)?;
        for reversal in &command.reversed_journals {
            sqlx::query("UPDATE sharing.bill_revision_accounting_journals SET ledger_reversal_journal_id=$1 WHERE user_id=$2 AND ledger_journal_id=$3 AND ledger_reversal_journal_id IS NULL")
                .bind(reversal.reversal_journal_id).bind(command.user_id.into_uuid()).bind(reversal.original_journal_id).execute(&mut *tx).await.map_err(database)?;
        }
        sqlx::query("UPDATE sharing.bill_revisions SET accounting_status='failed',last_error=$1 WHERE bill_id=$2 AND user_id=$3 AND revision=$4")
            .bind(truncate_error(&command.error)).bind(command.bill_id.into_uuid()).bind(command.user_id.into_uuid()).bind(i32::try_from(command.revision).map_err(|_|SharingError::ArithmeticOverflow)?).execute(&mut *tx).await.map_err(database)?;
        sqlx::query("UPDATE sharing.bills SET status='failed',version=version+1,updated_at=$1 WHERE id=$2 AND user_id=$3")
            .bind(command.occurred_at).bind(command.bill_id.into_uuid()).bind(command.user_id.into_uuid()).execute(&mut *tx).await.map_err(database)?;
        finish_failed_process(&mut tx, &command.claim, &command.error).await?;
        let view = load_bill_tx(&mut tx, command.user_id, command.bill_id)
            .await?
            .ok_or(SharingError::NotFound)?;
        tx.commit().await.map_err(database)?;
        Ok(view)
    }

    pub(crate) async fn fail_settlement_accounting(
        &self,
        command: FailSettlementAccounting,
    ) -> Result<SettlementView, SharingError> {
        let mut tx = self.pool.begin().await.map_err(database)?;
        verify_workflow_claim(&mut tx, &command.claim).await?;
        let row=sqlx::query("SELECT amount,currency,version,obligation_id FROM sharing.settlements WHERE id=$1 AND bill_id=$2 AND user_id=$3 AND status='pending_accounting' FOR UPDATE")
            .bind(command.settlement_id.into_uuid()).bind(command.bill_id.into_uuid()).bind(command.user_id.into_uuid()).fetch_optional(&mut *tx).await.map_err(database)?.ok_or(SharingError::NotFound)?;
        let actual = u64::try_from(row.get::<i64, _>("version"))
            .map_err(|_| SharingError::ArithmeticOverflow)?;
        if actual != command.expected_version.0 {
            return Err(SharingError::VersionConflict {
                expected: command.expected_version.0,
                actual,
            });
        }
        sqlx::query("UPDATE sharing.settlements SET status='failed',version=version+1,last_error=$1 WHERE id=$2 AND user_id=$3")
            .bind(truncate_error(&command.error)).bind(command.settlement_id.into_uuid()).bind(command.user_id.into_uuid()).execute(&mut *tx).await.map_err(database)?;
        sqlx::query("UPDATE sharing.obligations SET settled_amount=settled_amount-$1 WHERE id=$2 AND user_id=$3 AND settled_amount>=$1")
            .bind(row.get::<Decimal,_>("amount")).bind(row.get::<Uuid,_>("obligation_id")).bind(command.user_id.into_uuid()).execute(&mut *tx).await.map_err(database)?;
        sqlx::query("UPDATE sharing.bills SET active_settlements=active_settlements-1,version=version+1,updated_at=$1 WHERE id=$2 AND user_id=$3 AND active_settlements>0")
            .bind(command.occurred_at).bind(command.bill_id.into_uuid()).bind(command.user_id.into_uuid()).execute(&mut *tx).await.map_err(database)?;
        finish_failed_process(&mut tx, &command.claim, &command.error).await?;
        let currency = CurrencyCode::new(row.get::<String, _>("currency"))
            .map_err(|error| SharingError::Persistence(error.to_string()))?;
        let view = SettlementView {
            id: command.settlement_id,
            bill_id: command.bill_id,
            debtor: None,
            creditor: None,
            amount: row.get("amount"),
            currency,
            evidence: None,
            occurred_at: None,
            accounting_journal_id: None,
            status: SettlementStatus::Failed,
            version: SettlementVersion(actual + 1),
            process: ProcessView {
                state: "failed".into(),
                correlation_id: load_process_correlation(&mut tx, &command.claim).await?,
                last_error: Some(truncate_error(&command.error)),
            },
        };
        tx.commit().await.map_err(database)?;
        Ok(view)
    }
}

#[async_trait]
impl crate::contexts::sharing::application::ports::ContactRepository for PgSharingStore {
    async fn create_contact(&self, command: CreateContact) -> Result<ContactResult, SharingError> {
        PgSharingStore::create_contact(self, command).await
    }

    async fn update_contact(&self, command: UpdateContact) -> Result<ContactResult, SharingError> {
        PgSharingStore::update_contact(self, command).await
    }

    async fn archive_contact(
        &self,
        command: ArchiveContact,
    ) -> Result<ContactResult, SharingError> {
        PgSharingStore::archive_contact(self, command).await
    }

    async fn contact(
        &self,
        user_id: UserId,
        id: ContactId,
    ) -> Result<Option<ContactView>, SharingError> {
        PgSharingStore::contact(self, user_id, id).await
    }

    async fn contacts(
        &self,
        user_id: UserId,
        include_archived: bool,
    ) -> Result<Vec<ContactView>, SharingError> {
        PgSharingStore::contacts(self, user_id, include_archived).await
    }
}

#[async_trait]
impl crate::contexts::sharing::application::ports::BillRepository for PgSharingStore {
    async fn create_bill(&self, command: CreateBillSplit) -> Result<BillResult, SharingError> {
        PgSharingStore::create_bill(self, command).await
    }

    async fn revise_bill(&self, command: ReviseBillSplit) -> Result<BillResult, SharingError> {
        PgSharingStore::revise_bill(self, command).await
    }

    async fn cancel_bill(&self, command: CancelBillSplit) -> Result<BillResult, SharingError> {
        PgSharingStore::cancel_bill(self, command).await
    }

    async fn bill(
        &self,
        user_id: UserId,
        id: BillSplitId,
    ) -> Result<Option<BillView>, SharingError> {
        PgSharingStore::bill(self, user_id, id).await
    }

    async fn bills(&self, user_id: UserId) -> Result<Vec<BillView>, SharingError> {
        PgSharingStore::bills(self, user_id).await
    }
}

#[async_trait]
impl crate::contexts::sharing::application::ports::SettlementRepository for PgSharingStore {
    async fn settlements(
        &self,
        user_id: UserId,
        bill_id: BillSplitId,
    ) -> Result<Vec<SettlementView>, SharingError> {
        PgSharingStore::settlements(self, user_id, bill_id).await
    }

    async fn create_settlement(
        &self,
        command: CreateSettlement,
    ) -> Result<SettlementResult, SharingError> {
        PgSharingStore::create_settlement(self, command).await
    }

    async fn reverse_settlement(
        &self,
        command: ReverseSettlement,
    ) -> Result<SettlementResult, SharingError> {
        PgSharingStore::reverse_settlement(self, command).await
    }
}

#[async_trait]
impl crate::contexts::sharing::application::ports::AccountingWorkflowRepository for PgSharingStore {
    async fn claim_next_work(
        &self,
        holder: &str,
    ) -> Result<Option<SharingWorkflowWork>, SharingError> {
        PgSharingStore::claim_next_work(self, holder).await
    }

    async fn retry_work(&self, command: RetrySharingWorkflow) -> Result<(), SharingError> {
        PgSharingStore::retry_work(self, command).await
    }

    async fn fail_bill_accounting(
        &self,
        command: FailBillAccounting,
    ) -> Result<BillView, SharingError> {
        PgSharingStore::fail_bill_accounting(self, command).await
    }

    async fn fail_settlement_accounting(
        &self,
        command: FailSettlementAccounting,
    ) -> Result<SettlementView, SharingError> {
        PgSharingStore::fail_settlement_accounting(self, command).await
    }

    async fn complete_bill_accounting(
        &self,
        command: CompleteBillAccounting,
    ) -> Result<BillView, SharingError> {
        PgSharingStore::complete_bill_accounting(self, command).await
    }

    async fn complete_bill_cancellation(
        &self,
        command: CompleteBillCancellation,
    ) -> Result<BillView, SharingError> {
        PgSharingStore::complete_bill_cancellation(self, command).await
    }

    async fn complete_settlement_accounting(
        &self,
        command: CompleteSettlementAccounting,
    ) -> Result<SettlementView, SharingError> {
        PgSharingStore::complete_settlement_accounting(self, command).await
    }

    async fn complete_settlement_reversal(
        &self,
        command: CompleteSettlementReversal,
    ) -> Result<SettlementView, SharingError> {
        PgSharingStore::complete_settlement_reversal(self, command).await
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
    let row=sqlx::query("SELECT b.id,b.user_id,b.currency,b.current_revision,b.status,b.active_settlements,b.version,r.title,r.occurred_at,r.total,(SELECT max(pr.revision) FROM sharing.bill_revisions pr WHERE pr.bill_id=b.id AND pr.user_id=b.user_id AND pr.accounting_status='posted') accounted_revision,r.last_error accounting_error,NOT EXISTS(SELECT 1 FROM sharing.obligations remaining WHERE remaining.bill_id=b.id AND remaining.user_id=b.user_id AND remaining.revision=b.current_revision AND remaining.settled_amount<remaining.original_amount) fully_settled,(SELECT jsonb_build_object('contributions',COALESCE(jsonb_agg(x.value ORDER BY x.position) FILTER(WHERE x.kind='contribution'),'[]'::jsonb),'shares',COALESCE(jsonb_agg(x.value ORDER BY x.position) FILTER(WHERE x.kind='share'),'[]'::jsonb),'obligations',COALESCE(jsonb_agg(x.value ORDER BY x.position) FILTER(WHERE x.kind='obligation'),'[]'::jsonb)) FROM (SELECT 'contribution' kind,c.position,jsonb_build_object('participant_kind',c.participant_kind,'contact_id',c.participant_contact_id,'amount',c.amount::text,'evidence',jsonb_strip_nulls(jsonb_build_object('kind',c.evidence_kind,'account_id',c.ledger_account_id,'allocations',CASE WHEN c.evidence_kind='existing_journals' THEN COALESCE((SELECT jsonb_agg(jsonb_build_object('journal_id',a.ledger_journal_id,'amount',a.amount::text,'currency',a.currency) ORDER BY a.position) FROM sharing.contribution_journal_allocations a WHERE a.contribution_id=c.id AND a.user_id=c.user_id),'[]'::jsonb) END))) value FROM sharing.contributions c WHERE c.bill_id=b.id AND c.user_id=b.user_id AND c.revision=b.current_revision UNION ALL SELECT 'share',s.position,jsonb_build_object('participant_kind',s.participant_kind,'contact_id',s.participant_contact_id,'amount',s.amount::text) FROM sharing.participant_shares s WHERE s.bill_id=b.id AND s.user_id=b.user_id AND s.revision=b.current_revision UNION ALL SELECT 'obligation',o.position,jsonb_build_object('id',o.id,'debtor_kind',o.debtor_kind,'debtor_contact_id',o.debtor_contact_id,'creditor_kind',o.creditor_kind,'creditor_contact_id',o.creditor_contact_id,'amount',o.original_amount::text,'settled_amount',o.settled_amount::text,'remaining_amount',(o.original_amount-o.settled_amount)::text) FROM sharing.obligations o WHERE o.bill_id=b.id AND o.user_id=b.user_id AND o.revision=b.current_revision) x) allocations FROM sharing.bills b JOIN sharing.bill_revisions r ON r.bill_id=b.id AND r.user_id=b.user_id AND r.revision=b.current_revision WHERE b.id=$1 AND b.user_id=$2").bind(id.into_uuid()).bind(user.into_uuid()).fetch_optional(&mut **tx).await.map_err(database)?;
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
        accounted_revision: row
            .get::<Option<i32>, _>("accounted_revision")
            .map(u32::try_from)
            .transpose()
            .map_err(|_| SharingError::ArithmeticOverflow)?,
        accounting_error: row.get("accounting_error"),
        status,
        version: BillVersion(
            u64::try_from(row.get::<i64, _>("version"))
                .map_err(|_| SharingError::ArithmeticOverflow)?,
        ),
        active_settlements: u32::try_from(row.get::<i32, _>("active_settlements"))
            .map_err(|_| SharingError::ArithmeticOverflow)?,
        fully_settled: row.get("fully_settled"),
        allocations: row
            .get::<Option<serde_json::Value>, _>("allocations")
            .unwrap_or_else(|| json!({})),
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
