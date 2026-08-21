//! In-process Phase 4 event router over the durable integration outbox.
//!
//! It owns an independent inbox receipt and does not acknowledge or claim the
//! transport-level outbox record, so it cannot steal events from a later
//! external publisher. Downstream consumers also deduplicate by event ID,
//! making a crash before the router receipt harmless.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{
    contexts::{
        ledger::public::{
            JournalEntryId, LedgerEventFactV1, LedgerEventMetadataV1, LedgerEventV1, LedgerFacade,
            LedgerMoneyV1,
        },
        mail::public::{
            RECEIPT_EVIDENCE_RECORDED_V1, ReceiptEvidenceId, ReceiptEvidenceKind,
            ReceiptEvidenceRecordedV1, SourceMessageId,
        },
        recurring::public::{
            CHARGE_EVIDENCE_RECORDED_V1, ChargeEvidenceId, ChargeEvidenceRecordedV1,
            RecurringFacade, SubscriptionId,
        },
        reference_data::public::{FX_OBSERVED_V1, FxObservedV1},
        reporting::public::ReportingFacade,
        sharing::public::{
            BILL_CANCELLED_V1, BILL_POSITION_CHANGED_V1, BillPositionV1, SharingEventFactV1,
            SharingEventMetadataV1, SharingEventV1,
        },
    },
    shared_kernel::{CausationId, CorrelationId, CurrencyCode, EventId, Money, UserId},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RouteReport {
    pub routed: bool,
    pub ignored: bool,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum RouteError {
    #[error("Phase 4 event routing persistence failed")]
    Database(#[from] sqlx::Error),
    #[error("Phase 4 event payload is invalid")]
    InvalidPayload,
    #[error("Phase 4 event major version is unsupported")]
    UnsupportedVersion,
    #[error("Phase 4 downstream consumer failed")]
    Consumer,
}

#[derive(Clone)]
pub(crate) struct Phase4EventRouter {
    pool: PgPool,
    ledger: LedgerFacade,
    recurring: RecurringFacade,
    reporting: ReportingFacade,
}

impl Phase4EventRouter {
    pub(crate) fn new(
        pool: PgPool,
        ledger: LedgerFacade,
        recurring: RecurringFacade,
        reporting: ReportingFacade,
    ) -> Self {
        Self {
            pool,
            ledger,
            recurring,
            reporting,
        }
    }

    pub(crate) async fn run_once(&self) -> Result<RouteReport, RouteError> {
        let Some(event) = self.next_event().await? else {
            return Ok(RouteReport::default());
        };
        if event.schema_version != 1 && is_phase4_event(&event.event_type) {
            return Err(RouteError::UnsupportedVersion);
        }
        let routed = self.route(&event).await?;
        sqlx::query(
            "INSERT INTO integration.inbox_receipts(consumer_name,message_id,event_type,received_at,processed_at) VALUES('finance-v2-phase4-router',$1,$2,clock_timestamp(),clock_timestamp()) ON CONFLICT(consumer_name,message_id) DO UPDATE SET processed_at=EXCLUDED.processed_at",
        )
        .bind(event.event_id)
        .bind(&event.event_type)
        .execute(&self.pool)
        .await?;
        Ok(RouteReport {
            routed,
            ignored: !routed,
        })
    }

    async fn next_event(&self) -> Result<Option<RoutedEvent>, sqlx::Error> {
        let row = sqlx::query(
            r#"
            SELECT o.sequence,o.event_id,o.message_schema_version,o.context_name,o.aggregate_id,
                   o.aggregate_version,o.event_type,o.user_id,o.occurred_at,o.correlation_id,
                   o.causation_id,o.payload
            FROM integration.outbox_messages o
            WHERE NOT EXISTS(
                SELECT 1 FROM integration.inbox_receipts i
                WHERE i.consumer_name='finance-v2-phase4-router' AND i.message_id=o.event_id
            )
            ORDER BY o.sequence,o.event_id LIMIT 1
            "#,
        )
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            let aggregate_version: i64 = row.get("aggregate_version");
            let schema_version: i32 = row.get("message_schema_version");
            let sequence: i64 = row.get("sequence");
            Ok(RoutedEvent {
                sequence: u64::try_from(sequence)
                    .map_err(|_| sqlx::Error::Protocol("negative outbox sequence".into()))?,
                event_id: row.get("event_id"),
                schema_version: u32::try_from(schema_version)
                    .map_err(|_| sqlx::Error::Protocol("negative event version".into()))?,
                context: row.get("context_name"),
                aggregate_id: row.get("aggregate_id"),
                aggregate_version: u64::try_from(aggregate_version)
                    .map_err(|_| sqlx::Error::Protocol("negative aggregate version".into()))?,
                event_type: row.get("event_type"),
                user_id: row.get("user_id"),
                occurred_at: row.get("occurred_at"),
                correlation_id: row.get("correlation_id"),
                causation_id: row.get("causation_id"),
                payload: row.get("payload"),
            })
        })
        .transpose()
    }

    async fn route(&self, event: &RoutedEvent) -> Result<bool, RouteError> {
        match event.event_type.as_str() {
            RECEIPT_EVIDENCE_RECORDED_V1 => {
                let evidence = mail_evidence(event)?;
                self.recurring
                    .consume_mail_evidence(event.event_id, event.sequence, evidence)
                    .await
                    .map_err(|_| RouteError::Consumer)?;
                Ok(true)
            }
            FX_OBSERVED_V1 => {
                self.reporting
                    .apply_fx_event(fx_event(event)?, event.sequence)
                    .await
                    .map_err(RouteError::Database)?;
                Ok(true)
            }
            CHARGE_EVIDENCE_RECORDED_V1 => {
                self.reporting
                    .apply_recurring_charge(
                        EventId::new(event.event_id),
                        event.sequence,
                        recurring_charge(event)?,
                    )
                    .await
                    .map_err(RouteError::Database)?;
                Ok(true)
            }
            "ledger.journal-posted.v1"
            | "ledger.journal-reversed.v1"
            | "ledger.journal-replaced.v1" => {
                let journal_id =
                    Uuid::parse_str(&event.aggregate_id).map_err(|_| RouteError::InvalidPayload)?;
                let journal = self
                    .ledger
                    .get_journal(UserId::new(event.user_id), JournalEntryId::new(journal_id))
                    .await
                    .map_err(|_| RouteError::Consumer)?;
                let fact = if event.event_type == "ledger.journal-reversed.v1" {
                    LedgerEventFactV1::EntryReversed {
                        journal_entry_id: journal.id,
                        original_journal_entry_id: journal
                            .relations
                            .reverses()
                            .ok_or(RouteError::InvalidPayload)?,
                    }
                } else if event.event_type == "ledger.journal-replaced.v1" {
                    LedgerEventFactV1::EntryReplaced {
                        replacement_journal_entry_id: journal.id,
                        original_journal_entry_id: journal
                            .relations
                            .replaces()
                            .ok_or(RouteError::InvalidPayload)?,
                    }
                } else {
                    LedgerEventFactV1::EntryPosted {
                        journal_entry_id: journal.id,
                        effects: journal
                            .postings
                            .iter()
                            .filter(|posting| {
                                posting.account_kind
                                    != crate::contexts::ledger::public::AccountKind::System
                            })
                            .map(|posting| LedgerMoneyV1 {
                                amount: posting.display_effect.abs(),
                                currency: posting.currency.clone(),
                            })
                            .collect(),
                    }
                };
                self.recurring
                    .consume_ledger_event(ledger_event(event, journal.recorded_at, fact))
                    .await
                    .map_err(|_| RouteError::Consumer)?;
                self.reporting
                    .apply_journal_export(EventId::new(event.event_id), event.sequence, journal)
                    .await
                    .map_err(RouteError::Database)?;
                Ok(true)
            }
            event_type if event_type.starts_with("ledger.reconciliation-") => {
                let case_id = payload_uuid(&event.payload, "case_id")?;
                let case_id = crate::contexts::ledger::public::ReconciliationCaseId::new(case_id);
                let fact = match event_type {
                    "ledger.reconciliation-observed.v1" => {
                        LedgerEventFactV1::ReconciliationObserved { case_id }
                    }
                    "ledger.reconciliation-matched.v1" => {
                        LedgerEventFactV1::ReconciliationMatched { case_id }
                    }
                    "ledger.reconciliation-superseded.v1" => {
                        LedgerEventFactV1::ReconciliationSuperseded { case_id }
                    }
                    "ledger.reconciliation-ignored-older.v1" => {
                        LedgerEventFactV1::ReconciliationIgnoredOlder { case_id }
                    }
                    "ledger.reconciliation-approved.v1" => {
                        LedgerEventFactV1::ReconciliationApproved {
                            case_id,
                            journal_entry_id: JournalEntryId::new(
                                payload_uuid(&event.payload, "journal_entry_id")
                                    .unwrap_or_else(|_| Uuid::nil()),
                            ),
                        }
                    }
                    "ledger.reconciliation-dismissed.v1" => {
                        LedgerEventFactV1::ReconciliationDismissed { case_id }
                    }
                    "ledger.reconciliation-stale.v1" => {
                        LedgerEventFactV1::ReconciliationStale { case_id }
                    }
                    _ => return Ok(false),
                };
                self.reporting
                    .apply_ledger_event(ledger_event(event, event.occurred_at, fact))
                    .await
                    .map_err(RouteError::Database)?;
                Ok(true)
            }
            BILL_POSITION_CHANGED_V1 => {
                #[derive(Deserialize)]
                struct Payload {
                    position: BillPositionV1,
                }
                let payload: Payload = serde_json::from_value(event.payload.clone())
                    .map_err(|_| RouteError::InvalidPayload)?;
                self.reporting
                    .apply_sharing_event(sharing_event(
                        event,
                        SharingEventFactV1::BillPositionChanged {
                            position: payload.position,
                        },
                    ))
                    .await
                    .map_err(RouteError::Database)?;
                Ok(true)
            }
            BILL_CANCELLED_V1 => {
                let fact: SharingEventFactV1 =
                    serde_json::from_value(json_to_tagged_fact("bill_cancelled", &event.payload))
                        .map_err(|_| RouteError::InvalidPayload)?;
                self.reporting
                    .apply_sharing_event(sharing_event(event, fact))
                    .await
                    .map_err(RouteError::Database)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

fn sharing_event(event: &RoutedEvent, fact: SharingEventFactV1) -> SharingEventV1 {
    SharingEventV1 {
        metadata: SharingEventMetadataV1 {
            schema_version: event.schema_version,
            event_id: EventId::new(event.event_id),
            user_id: UserId::new(event.user_id),
            sequence: event.sequence,
            correlation_id: CorrelationId::new(event.correlation_id),
            causation_id: event.causation_id.map(CausationId::new),
            occurred_at: event.occurred_at,
            recorded_at: event.occurred_at,
        },
        fact,
    }
}

fn json_to_tagged_fact(kind: &str, payload: &serde_json::Value) -> serde_json::Value {
    let mut object = payload.as_object().cloned().unwrap_or_default();
    object.insert(
        "type".to_owned(),
        serde_json::Value::String(kind.to_owned()),
    );
    serde_json::Value::Object(object)
}

fn ledger_event(
    event: &RoutedEvent,
    recorded_at: DateTime<Utc>,
    fact: LedgerEventFactV1,
) -> LedgerEventV1 {
    LedgerEventV1 {
        metadata: LedgerEventMetadataV1 {
            schema_version: event.schema_version,
            event_id: EventId::new(event.event_id),
            user_id: UserId::new(event.user_id),
            sequence: event.sequence,
            correlation_id: CorrelationId::new(event.correlation_id),
            causation_id: event.causation_id.map(CausationId::new),
            occurred_at: event.occurred_at,
            recorded_at,
        },
        fact,
    }
}

fn mail_evidence(event: &RoutedEvent) -> Result<ReceiptEvidenceRecordedV1, RouteError> {
    #[derive(Deserialize)]
    struct Wire {
        evidence_id: Uuid,
        user_id: Uuid,
        source_message_id: Uuid,
        merchant: String,
        kind: String,
        money: Option<WireMoney>,
        charged_at: Option<DateTime<Utc>>,
        parser_name: String,
        parser_version: u32,
        provenance_digest: [u8; 32],
        recorded_at: DateTime<Utc>,
    }
    let wire: Wire =
        serde_json::from_value(event.payload.clone()).map_err(|_| RouteError::InvalidPayload)?;
    Ok(ReceiptEvidenceRecordedV1 {
        evidence_id: ReceiptEvidenceId::new(wire.evidence_id),
        user_id: UserId::new(wire.user_id),
        source_message_id: SourceMessageId::new(wire.source_message_id),
        merchant: wire.merchant,
        kind: match wire.kind.as_str() {
            "renewal" => ReceiptEvidenceKind::Renewal,
            "one_time" => ReceiptEvidenceKind::OneTime,
            "refund" => ReceiptEvidenceKind::Refund,
            "cancellation" => ReceiptEvidenceKind::Cancellation,
            _ => return Err(RouteError::InvalidPayload),
        },
        money: wire.money.map(money).transpose()?,
        charged_at: wire.charged_at,
        parser_name: wire.parser_name,
        parser_version: wire.parser_version,
        provenance_digest: wire.provenance_digest,
        recorded_at: wire.recorded_at,
    })
}

fn fx_event(event: &RoutedEvent) -> Result<FxObservedV1, RouteError> {
    #[derive(Deserialize)]
    struct Wire {
        observation_id: Uuid,
        source: String,
        source_revision: String,
        base_currency: String,
        quote_currency: String,
        rate: String,
        effective_at: DateTime<Utc>,
        observed_at: DateTime<Utc>,
        recorded_at: DateTime<Utc>,
    }
    let wire: Wire =
        serde_json::from_value(event.payload.clone()).map_err(|_| RouteError::InvalidPayload)?;
    Ok(FxObservedV1 {
        observation_id: wire.observation_id,
        source: wire.source,
        source_revision: wire.source_revision,
        base_currency: CurrencyCode::new(wire.base_currency)
            .map_err(|_| RouteError::InvalidPayload)?,
        quote_currency: CurrencyCode::new(wire.quote_currency)
            .map_err(|_| RouteError::InvalidPayload)?,
        rate: wire
            .rate
            .parse::<Decimal>()
            .map_err(|_| RouteError::InvalidPayload)?,
        effective_at: wire.effective_at,
        observed_at: wire.observed_at,
        recorded_at: wire.recorded_at,
    })
}

fn recurring_charge(event: &RoutedEvent) -> Result<ChargeEvidenceRecordedV1, RouteError> {
    #[derive(Deserialize)]
    struct Wire {
        user_id: Uuid,
        charge_evidence_id: Uuid,
        subscription_id: Uuid,
        merchant: String,
        money: Option<WireMoney>,
        charged_at: Option<DateTime<Utc>>,
        recorded_at: DateTime<Utc>,
    }
    let wire: Wire =
        serde_json::from_value(event.payload.clone()).map_err(|_| RouteError::InvalidPayload)?;
    Ok(ChargeEvidenceRecordedV1 {
        user_id: UserId::new(wire.user_id),
        charge_evidence_id: ChargeEvidenceId::new(wire.charge_evidence_id),
        subscription_id: SubscriptionId::new(wire.subscription_id),
        merchant: wire.merchant,
        money: wire.money.map(money).transpose()?,
        charged_at: wire.charged_at,
        recorded_at: wire.recorded_at,
    })
}

#[derive(Deserialize)]
struct WireMoney {
    amount: String,
    currency: String,
}

fn money(wire: WireMoney) -> Result<Money, RouteError> {
    let amount = wire
        .amount
        .parse::<Decimal>()
        .map_err(|_| RouteError::InvalidPayload)?;
    Money::new(
        amount,
        CurrencyCode::new(wire.currency).map_err(|_| RouteError::InvalidPayload)?,
        amount.scale(),
    )
    .map_err(|_| RouteError::InvalidPayload)
}

fn payload_uuid(payload: &serde_json::Value, key: &str) -> Result<Uuid, RouteError> {
    payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .ok_or(RouteError::InvalidPayload)
        .and_then(|value| Uuid::parse_str(value).map_err(|_| RouteError::InvalidPayload))
}

fn is_phase4_event(event_type: &str) -> bool {
    event_type.starts_with("ledger.")
        || event_type.starts_with("mail.")
        || event_type.starts_with("recurring.")
        || event_type == FX_OBSERVED_V1
}

struct RoutedEvent {
    sequence: u64,
    event_id: Uuid,
    schema_version: u32,
    #[allow(dead_code)]
    context: String,
    aggregate_id: String,
    #[allow(dead_code)]
    aggregate_version: u64,
    event_type: String,
    user_id: Uuid,
    occurred_at: DateTime<Utc>,
    correlation_id: Uuid,
    causation_id: Option<Uuid>,
    payload: serde_json::Value,
}
