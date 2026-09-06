//! Independent cross-context policies over the durable integration outbox.
//!
//! Recurring and Reporting own separate receipts and therefore make progress
//! independently. They do not acknowledge the transport-level outbox record;
//! each target context deduplicates by event identity before the feed receipt
//! is recorded.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use sqlx::{PgPool, Row};
use tracing::Instrument as _;
use uuid::Uuid;

use crate::{
    contexts::{
        ledger::public::{
            CATEGORY_ASSIGNMENT_CHANGED_V1, JOURNAL_POSTED_V1, JOURNAL_REPLACED_V1,
            JOURNAL_REVERSED_V1, JournalEntryId, LedgerEventFactV1, LedgerEventMetadataV1,
            LedgerEventV1, LedgerFacade, LedgerMoneyV1, RECONCILIATION_APPROVED_V1,
            RECONCILIATION_DISMISSED_V1, RECONCILIATION_IGNORED_OLDER_V1,
            RECONCILIATION_MATCHED_V1, RECONCILIATION_OBSERVED_V1, RECONCILIATION_STALE_V1,
            RECONCILIATION_SUPERSEDED_V1,
        },
        loans::public::{
            ACCOUNTING_REQUESTED_V1, AGREEMENT_CLOSED_V1, AGREEMENT_OPENED_V1, MOVEMENT_FAILED_V1,
            MOVEMENT_POSTED_V1, MOVEMENT_REVERSED_V1, TERMS_REVISED_V1,
        },
        mail::public::{
            RECEIPT_EVIDENCE_RECORDED_V1, ReceiptEvidenceId, ReceiptEvidenceKind,
            ReceiptEvidenceRecordedV1, SourceMessageId,
        },
        portfolio::public::{
            ACCOUNT_CHANGED_V1, AccountLifecycle as PortfolioAccountLifecycle,
            CASH_SETTLEMENT_CANCELLED_V1, CASH_SETTLEMENT_POSTED_V1, CASH_SETTLEMENT_REVERSED_V1,
            INSTRUMENT_CREATED_V1, InstrumentId, POSITION_CHANGED_V1, PortfolioAccountId,
            PortfolioEventFactV1, PortfolioEventMetadataV1, PortfolioEventV1,
            PortfolioTransactionId, PortfolioTransactionKind, TRANSACTION_POSTED_V1,
            TRANSACTION_REVERSED_V1, VALUATION_RECORDED_V1, ValuationSnapshotId,
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

const RECURRING_CONSUMER_NAME: &str = "recurring-event-policy-v1";
const REPORTING_CONSUMER_NAME: &str = "reporting-projections-v1";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConsumerRunReport {
    pub applied: bool,
    pub ignored: bool,
}

impl ConsumerRunReport {
    pub const fn claimed(self) -> bool {
        self.applied || self.ignored
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ConsumerError {
    #[error("event-consumer persistence failed")]
    Database(#[from] sqlx::Error),
    #[error("published event payload is invalid")]
    InvalidPayload,
    #[error("published event schema version is unsupported")]
    UnsupportedVersion,
    #[error("downstream context rejected the published event")]
    Consumer,
    #[error("Reporting rejected the published event")]
    Reporting(#[from] crate::contexts::reporting::public::ReportingError),
}

#[derive(Clone)]
struct EventFeed {
    pool: PgPool,
    consumer_name: &'static str,
}

impl EventFeed {
    fn new(pool: PgPool, consumer_name: &'static str) -> Self {
        Self {
            pool,
            consumer_name,
        }
    }

    async fn next_event(&self) -> Result<Option<PersistedEvent>, sqlx::Error> {
        let row = sqlx::query(
            r#"
            SELECT o.sequence,o.event_id,o.message_schema_version,o.context_name,o.aggregate_id,
                   o.aggregate_version,o.event_type,o.user_id,o.occurred_at,o.correlation_id,
                   o.causation_id,o.payload
            FROM integration.outbox_messages o
            WHERE NOT EXISTS(
                SELECT 1 FROM integration.inbox_receipts i
                WHERE i.consumer_name=$1 AND i.message_id=o.event_id
            )
            ORDER BY o.sequence,o.event_id LIMIT 1
            "#,
        )
        .bind(self.consumer_name)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            let aggregate_version: i64 = row.get("aggregate_version");
            let schema_version: i32 = row.get("message_schema_version");
            let sequence: i64 = row.get("sequence");
            Ok(PersistedEvent {
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

    async fn acknowledge(&self, event: &PersistedEvent) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO integration.inbox_receipts(consumer_name,message_id,event_type,received_at,processed_at) VALUES($1,$2,$3,clock_timestamp(),clock_timestamp()) ON CONFLICT(consumer_name,message_id) DO UPDATE SET processed_at=EXCLUDED.processed_at",
        )
        .bind(self.consumer_name)
        .bind(event.event_id)
        .bind(&event.event_type)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct RecurringEventConsumer {
    feed: EventFeed,
    ledger: LedgerFacade,
    recurring: RecurringFacade,
}

impl RecurringEventConsumer {
    pub(crate) fn new(pool: PgPool, ledger: LedgerFacade, recurring: RecurringFacade) -> Self {
        Self {
            feed: EventFeed::new(pool, RECURRING_CONSUMER_NAME),
            ledger,
            recurring,
        }
    }

    pub(crate) async fn run_once(&self) -> Result<ConsumerRunReport, ConsumerError> {
        let Some(event) = self.feed.next_event().await? else {
            return Ok(ConsumerRunReport::default());
        };
        let item_span = event_span("integration.recurring_consumer", &event);
        log_claimed(&item_span);
        async {
            if event.schema_version != 1 && is_recurring_event(&event.event_type) {
                return Err(ConsumerError::UnsupportedVersion);
            }
            let applied = self.consume(&event).await?;
            self.feed.acknowledge(&event).await?;
            Ok(ConsumerRunReport {
                applied,
                ignored: !applied,
            })
        }
        .instrument(item_span)
        .await
    }

    async fn consume(&self, event: &PersistedEvent) -> Result<bool, ConsumerError> {
        match event.event_type.as_str() {
            RECEIPT_EVIDENCE_RECORDED_V1 => {
                let evidence = mail_evidence(event)?;
                self.recurring
                    .consume_mail_evidence(event.event_id, event.sequence, evidence)
                    .await
                    .map_err(|_| ConsumerError::Consumer)?;
                Ok(true)
            }
            JOURNAL_POSTED_V1 | JOURNAL_REVERSED_V1 | JOURNAL_REPLACED_V1 => {
                let journal_id = Uuid::parse_str(&event.aggregate_id)
                    .map_err(|_| ConsumerError::InvalidPayload)?;
                let journal = self
                    .ledger
                    .get_journal(UserId::new(event.user_id), JournalEntryId::new(journal_id))
                    .await
                    .map_err(|_| ConsumerError::Consumer)?;
                let fact = if event.event_type == JOURNAL_REVERSED_V1 {
                    LedgerEventFactV1::EntryReversed {
                        journal_entry_id: journal.id,
                        original_journal_entry_id: journal
                            .relations
                            .reverses()
                            .ok_or(ConsumerError::InvalidPayload)?,
                    }
                } else if event.event_type == JOURNAL_REPLACED_V1 {
                    LedgerEventFactV1::EntryReplaced {
                        replacement_journal_entry_id: journal.id,
                        original_journal_entry_id: journal
                            .relations
                            .replaces()
                            .ok_or(ConsumerError::InvalidPayload)?,
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
                    .map_err(|_| ConsumerError::Consumer)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

#[derive(Clone)]
pub(crate) struct ReportingEventConsumer {
    feed: EventFeed,
    ledger: LedgerFacade,
    reporting: ReportingFacade,
}

impl ReportingEventConsumer {
    pub(crate) fn new(pool: PgPool, ledger: LedgerFacade, reporting: ReportingFacade) -> Self {
        Self {
            feed: EventFeed::new(pool, REPORTING_CONSUMER_NAME),
            ledger,
            reporting,
        }
    }

    pub(crate) async fn run_once(&self) -> Result<ConsumerRunReport, ConsumerError> {
        let Some(event) = self.feed.next_event().await? else {
            return Ok(ConsumerRunReport::default());
        };
        let item_span = event_span("integration.reporting_consumer", &event);
        log_claimed(&item_span);
        async {
            if event.schema_version != 1 && is_reporting_event(&event.event_type) {
                return Err(ConsumerError::UnsupportedVersion);
            }
            let applied = self.consume(&event).await?;
            self.feed.acknowledge(&event).await?;
            Ok(ConsumerRunReport {
                applied,
                ignored: !applied,
            })
        }
        .instrument(item_span)
        .await
    }

    async fn consume(&self, event: &PersistedEvent) -> Result<bool, ConsumerError> {
        match event.event_type.as_str() {
            FX_OBSERVED_V1 => {
                self.reporting
                    .apply_fx_event(fx_event(event)?, event.sequence)
                    .await
                    .map_err(ConsumerError::Reporting)?;
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
                    .map_err(ConsumerError::Reporting)?;
                Ok(true)
            }
            JOURNAL_POSTED_V1 | JOURNAL_REVERSED_V1 | JOURNAL_REPLACED_V1 => {
                let journal_id = Uuid::parse_str(&event.aggregate_id)
                    .map_err(|_| ConsumerError::InvalidPayload)?;
                let journal = self
                    .ledger
                    .get_journal(UserId::new(event.user_id), JournalEntryId::new(journal_id))
                    .await
                    .map_err(|_| ConsumerError::Consumer)?;
                self.reporting
                    .apply_journal_export(EventId::new(event.event_id), event.sequence, journal)
                    .await
                    .map_err(ConsumerError::Reporting)?;
                Ok(true)
            }
            CATEGORY_ASSIGNMENT_CHANGED_V1 => {
                let fact: LedgerEventFactV1 = serde_json::from_value(json_to_tagged_fact(
                    "category_assignment_changed",
                    &event.payload,
                ))
                .map_err(|_| ConsumerError::InvalidPayload)?;
                self.reporting
                    .apply_ledger_event(ledger_event(event, event.occurred_at, fact))
                    .await
                    .map_err(ConsumerError::Reporting)?;
                Ok(true)
            }
            RECONCILIATION_OBSERVED_V1
            | RECONCILIATION_MATCHED_V1
            | RECONCILIATION_SUPERSEDED_V1
            | RECONCILIATION_IGNORED_OLDER_V1
            | RECONCILIATION_APPROVED_V1
            | RECONCILIATION_DISMISSED_V1
            | RECONCILIATION_STALE_V1 => {
                let event_type = event.event_type.as_str();
                let case_id = payload_uuid(&event.payload, "case_id")?;
                let case_id = crate::contexts::ledger::public::ReconciliationCaseId::new(case_id);
                let fact = match event_type {
                    RECONCILIATION_OBSERVED_V1 => {
                        LedgerEventFactV1::ReconciliationObserved { case_id }
                    }
                    RECONCILIATION_MATCHED_V1 => {
                        LedgerEventFactV1::ReconciliationMatched { case_id }
                    }
                    RECONCILIATION_SUPERSEDED_V1 => {
                        LedgerEventFactV1::ReconciliationSuperseded { case_id }
                    }
                    RECONCILIATION_IGNORED_OLDER_V1 => {
                        LedgerEventFactV1::ReconciliationIgnoredOlder { case_id }
                    }
                    RECONCILIATION_APPROVED_V1 => LedgerEventFactV1::ReconciliationApproved {
                        case_id,
                        journal_entry_id: JournalEntryId::new(
                            payload_uuid(&event.payload, "journal_entry_id")
                                .unwrap_or_else(|_| Uuid::nil()),
                        ),
                    },
                    RECONCILIATION_DISMISSED_V1 => {
                        LedgerEventFactV1::ReconciliationDismissed { case_id }
                    }
                    RECONCILIATION_STALE_V1 => LedgerEventFactV1::ReconciliationStale { case_id },
                    _ => return Ok(false),
                };
                self.reporting
                    .apply_ledger_event(ledger_event(event, event.occurred_at, fact))
                    .await
                    .map_err(ConsumerError::Reporting)?;
                Ok(true)
            }
            event_type if is_loan_event(event_type) => {
                let mut loan_event: crate::contexts::loans::public::LoanEventV1 =
                    serde_json::from_value(event.payload.clone())
                        .map_err(|_| ConsumerError::InvalidPayload)?;
                loan_event.metadata.sequence = event.sequence;
                self.reporting
                    .apply_loan_event(loan_event)
                    .await
                    .map_err(ConsumerError::Reporting)?;
                Ok(true)
            }
            BILL_POSITION_CHANGED_V1 => {
                #[derive(Deserialize)]
                struct Payload {
                    position: BillPositionV1,
                }
                let payload: Payload = serde_json::from_value(event.payload.clone())
                    .map_err(|_| ConsumerError::InvalidPayload)?;
                self.reporting
                    .apply_sharing_event(sharing_event(
                        event,
                        SharingEventFactV1::BillPositionChanged {
                            position: payload.position,
                        },
                    ))
                    .await
                    .map_err(ConsumerError::Reporting)?;
                Ok(true)
            }
            BILL_CANCELLED_V1 => {
                let fact: SharingEventFactV1 =
                    serde_json::from_value(json_to_tagged_fact("bill_cancelled", &event.payload))
                        .map_err(|_| ConsumerError::InvalidPayload)?;
                self.reporting
                    .apply_sharing_event(sharing_event(event, fact))
                    .await
                    .map_err(ConsumerError::Reporting)?;
                Ok(true)
            }
            event_type if is_portfolio_event(event_type) => {
                self.reporting
                    .apply_portfolio_event(portfolio_event(event)?)
                    .await
                    .map_err(ConsumerError::Reporting)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

fn sharing_event(event: &PersistedEvent, fact: SharingEventFactV1) -> SharingEventV1 {
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

fn portfolio_event(event: &PersistedEvent) -> Result<PortfolioEventV1, ConsumerError> {
    let text = |key: &str| {
        event
            .payload
            .get(key)
            .and_then(serde_json::Value::as_str)
            .ok_or(ConsumerError::InvalidPayload)
    };
    let decimal = |key: &str| {
        text(key)?
            .parse::<Decimal>()
            .map_err(|_| ConsumerError::InvalidPayload)
    };
    let account = || payload_uuid(&event.payload, "account_id").map(PortfolioAccountId::new);
    let instrument = || payload_uuid(&event.payload, "instrument_id").map(InstrumentId::new);
    let transaction =
        || payload_uuid(&event.payload, "transaction_id").map(PortfolioTransactionId::new);
    let fact = match event.event_type.as_str() {
        INSTRUMENT_CREATED_V1 => PortfolioEventFactV1::InstrumentCreated {
            instrument_id: instrument()?,
        },
        ACCOUNT_CHANGED_V1 => PortfolioEventFactV1::AccountChanged {
            account_id: account()?,
            lifecycle: if text("lifecycle")? == "active" {
                PortfolioAccountLifecycle::Active
            } else {
                PortfolioAccountLifecycle::Archived
            },
        },
        TRANSACTION_POSTED_V1 => PortfolioEventFactV1::TransactionPosted {
            transaction_id: transaction()?,
            account_id: account()?,
            instrument_id: instrument()?,
            kind: transaction_kind(text("kind")?)?,
            quantity: decimal("quantity")?,
            currency: text("currency")?.to_owned(),
        },
        TRANSACTION_REVERSED_V1 => PortfolioEventFactV1::TransactionReversed {
            transaction_id: transaction()?,
            original_transaction_id: payload_uuid(&event.payload, "original_transaction_id")
                .map(PortfolioTransactionId::new)?,
        },
        POSITION_CHANGED_V1 => PortfolioEventFactV1::PositionChanged {
            account_id: account()?,
            instrument_id: instrument()?,
            quantity: decimal("quantity")?,
            known_cost_quantity: decimal("known_cost_quantity")?,
            unknown_cost_quantity: decimal("unknown_cost_quantity")?,
            remaining_known_cost: decimal("remaining_known_cost")?,
            realized_gain_loss: event
                .payload
                .get("realized_gain_loss")
                .and_then(serde_json::Value::as_str)
                .map(str::parse)
                .transpose()
                .map_err(|_| ConsumerError::InvalidPayload)?,
            currency: text("currency")?.to_owned(),
            position_version: event
                .payload
                .get("position_version")
                .and_then(serde_json::Value::as_u64)
                .ok_or(ConsumerError::InvalidPayload)?,
        },
        VALUATION_RECORDED_V1 => PortfolioEventFactV1::ValuationRecorded {
            snapshot_id: payload_uuid(&event.payload, "snapshot_id")
                .map(ValuationSnapshotId::new)?,
            account_id: account()?,
            instrument_id: instrument()?,
            quantity: decimal("quantity")?,
            price_per_instrument: decimal("price_per_instrument")?,
            accrued_interest_per_instrument: decimal("accrued_interest_per_instrument")?,
            market_value: decimal("market_value")?,
            currency: text("currency")?.to_owned(),
            quoted_at: serde_json::from_value(
                event
                    .payload
                    .get("quoted_at")
                    .cloned()
                    .ok_or(ConsumerError::InvalidPayload)?,
            )
            .map_err(|_| ConsumerError::InvalidPayload)?,
            source: text("source")?.to_owned(),
        },
        CASH_SETTLEMENT_POSTED_V1 => PortfolioEventFactV1::CashSettlementPosted {
            transaction_id: transaction()?,
            journal_id: payload_uuid(&event.payload, "journal_id").map(JournalEntryId::new)?,
        },
        CASH_SETTLEMENT_REVERSED_V1 => PortfolioEventFactV1::CashSettlementReversed {
            transaction_id: transaction()?,
            journal_id: payload_uuid(&event.payload, "journal_id").map(JournalEntryId::new)?,
            reversal_journal_id: payload_uuid(&event.payload, "reversal_journal_id")
                .map(JournalEntryId::new)?,
        },
        CASH_SETTLEMENT_CANCELLED_V1 => {
            PortfolioEventFactV1::CashSettlementCancelledWithoutEffect {
                transaction_id: transaction()?,
            }
        }
        _ => return Err(ConsumerError::InvalidPayload),
    };
    Ok(PortfolioEventV1 {
        metadata: PortfolioEventMetadataV1 {
            schema_version: event.schema_version,
            event_id: EventId::new(event.event_id),
            user_id: UserId::new(event.user_id),
            sequence: event.sequence,
            correlation_id: CorrelationId::new(event.correlation_id),
            occurred_at: event.occurred_at,
            recorded_at: event.occurred_at,
        },
        fact,
    })
}
fn transaction_kind(value: &str) -> Result<PortfolioTransactionKind, ConsumerError> {
    Ok(match value {
        "opening_position" => PortfolioTransactionKind::OpeningPosition,
        "buy" => PortfolioTransactionKind::Buy,
        "sell" => PortfolioTransactionKind::Sell,
        "coupon" => PortfolioTransactionKind::Coupon,
        "redemption" => PortfolioTransactionKind::Redemption,
        "position_correction" => PortfolioTransactionKind::PositionCorrection,
        "reversal" => PortfolioTransactionKind::Reversal,
        _ => return Err(ConsumerError::InvalidPayload),
    })
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
    event: &PersistedEvent,
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

fn mail_evidence(event: &PersistedEvent) -> Result<ReceiptEvidenceRecordedV1, ConsumerError> {
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
        serde_json::from_value(event.payload.clone()).map_err(|_| ConsumerError::InvalidPayload)?;
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
            _ => return Err(ConsumerError::InvalidPayload),
        },
        money: wire.money.map(money).transpose()?,
        charged_at: wire.charged_at,
        parser_name: wire.parser_name,
        parser_version: wire.parser_version,
        provenance_digest: wire.provenance_digest,
        recorded_at: wire.recorded_at,
    })
}

fn fx_event(event: &PersistedEvent) -> Result<FxObservedV1, ConsumerError> {
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
        serde_json::from_value(event.payload.clone()).map_err(|_| ConsumerError::InvalidPayload)?;
    Ok(FxObservedV1 {
        observation_id: wire.observation_id,
        source: wire.source,
        source_revision: wire.source_revision,
        base_currency: CurrencyCode::new(wire.base_currency)
            .map_err(|_| ConsumerError::InvalidPayload)?,
        quote_currency: CurrencyCode::new(wire.quote_currency)
            .map_err(|_| ConsumerError::InvalidPayload)?,
        rate: wire
            .rate
            .parse::<Decimal>()
            .map_err(|_| ConsumerError::InvalidPayload)?,
        effective_at: wire.effective_at,
        observed_at: wire.observed_at,
        recorded_at: wire.recorded_at,
    })
}

fn recurring_charge(event: &PersistedEvent) -> Result<ChargeEvidenceRecordedV1, ConsumerError> {
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
        serde_json::from_value(event.payload.clone()).map_err(|_| ConsumerError::InvalidPayload)?;
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

fn money(wire: WireMoney) -> Result<Money, ConsumerError> {
    let amount = wire
        .amount
        .parse::<Decimal>()
        .map_err(|_| ConsumerError::InvalidPayload)?;
    Money::new(
        amount,
        CurrencyCode::new(wire.currency).map_err(|_| ConsumerError::InvalidPayload)?,
        amount.scale(),
    )
    .map_err(|_| ConsumerError::InvalidPayload)
}

fn payload_uuid(payload: &serde_json::Value, key: &str) -> Result<Uuid, ConsumerError> {
    payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .ok_or(ConsumerError::InvalidPayload)
        .and_then(|value| Uuid::parse_str(value).map_err(|_| ConsumerError::InvalidPayload))
}

fn is_recurring_event(event_type: &str) -> bool {
    matches!(
        event_type,
        RECEIPT_EVIDENCE_RECORDED_V1
            | JOURNAL_POSTED_V1
            | JOURNAL_REVERSED_V1
            | JOURNAL_REPLACED_V1
    )
}

fn is_loan_event(event_type: &str) -> bool {
    matches!(
        event_type,
        AGREEMENT_OPENED_V1
            | TERMS_REVISED_V1
            | MOVEMENT_POSTED_V1
            | MOVEMENT_FAILED_V1
            | MOVEMENT_REVERSED_V1
            | AGREEMENT_CLOSED_V1
            | ACCOUNTING_REQUESTED_V1
    )
}

fn is_portfolio_event(event_type: &str) -> bool {
    matches!(
        event_type,
        INSTRUMENT_CREATED_V1
            | ACCOUNT_CHANGED_V1
            | TRANSACTION_POSTED_V1
            | TRANSACTION_REVERSED_V1
            | POSITION_CHANGED_V1
            | VALUATION_RECORDED_V1
            | CASH_SETTLEMENT_POSTED_V1
            | CASH_SETTLEMENT_REVERSED_V1
            | CASH_SETTLEMENT_CANCELLED_V1
    )
}

fn is_reporting_event(event_type: &str) -> bool {
    matches!(
        event_type,
        FX_OBSERVED_V1
            | CHARGE_EVIDENCE_RECORDED_V1
            | JOURNAL_POSTED_V1
            | JOURNAL_REVERSED_V1
            | JOURNAL_REPLACED_V1
            | CATEGORY_ASSIGNMENT_CHANGED_V1
            | RECONCILIATION_OBSERVED_V1
            | RECONCILIATION_MATCHED_V1
            | RECONCILIATION_SUPERSEDED_V1
            | RECONCILIATION_IGNORED_OLDER_V1
            | RECONCILIATION_APPROVED_V1
            | RECONCILIATION_DISMISSED_V1
            | RECONCILIATION_STALE_V1
            | BILL_POSITION_CHANGED_V1
            | BILL_CANCELLED_V1
    ) || is_loan_event(event_type)
        || is_portfolio_event(event_type)
}

struct PersistedEvent {
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

fn event_span(operation: &'static str, event: &PersistedEvent) -> tracing::Span {
    tracing::info_span!(
        "worker.item",
        operation,
        event_id = %event.event_id,
        correlation_id = %event.correlation_id,
    )
}

fn log_claimed(span: &tracing::Span) {
    span.in_scope(|| {
        tracing::info!(
            event.name = "worker.item.claimed",
            outcome = "claimed",
            "Worker item claimed"
        );
    });
}
