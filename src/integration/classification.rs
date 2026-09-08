//! Durable cross-context policies for asynchronous transaction classification.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use chrono::Utc;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::contexts::banking::public::{
    BankingFacade, PROVIDER_TRANSACTION_IMPORTED_V1, ProviderClassificationEvidence,
    ProviderTransactionImportedV1,
};
use crate::contexts::classification::public::{
    ApplicationClaim, BackfillClaim, CategoryCatalog, CategoryCatalogFacade, CategoryKind,
    CategoryNodeView, ClassificationAutomationFacade, ClassificationCategory,
    ClassificationCategoryKind, ClassificationEvidence, ClassificationEvidenceInput,
    ClassificationWorkFacade, ClassificationWorker, FeedbackSignal, StaleTarget, TargetOrigin,
    TransactionClassifier,
};
use crate::contexts::ledger::public::{
    AccountKind, AccountNature, ActivityCursor, ActivityFilter, ActivityKind, AnnotationVersion,
    ApplyCategoryAssignment, AssignmentOrigin, AutomationState, CATEGORY_ASSIGNMENT_CHANGED_V1,
    JOURNAL_POSTED_V1, JOURNAL_REPLACED_V1, JOURNAL_REVERSED_V1, JournalEntryId, JournalSource,
    JournalView, LedgerError, LedgerFacade, PostingPurpose,
};
use crate::shared_kernel::{CorrelationId, IdempotencyKey, UserId};

const CONSUMER_NAME: &str = "classification-intake-v1";
const ANNOTATION_CHANGED_V1: &str = "ledger.annotation-changed.v1";
const MAX_EXAMPLES: usize = 20;

#[cfg(test)]
#[path = "classification_tests.rs"]
mod tests;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ClassificationRunReport {
    pub(crate) claimed: bool,
    pub(crate) records: u32,
    pub(crate) retry_scheduled: bool,
    pub(crate) fenced: bool,
    pub(crate) failed: bool,
}

#[derive(Clone)]
pub(crate) struct ClassificationRuntime {
    intake: ClassificationIntakeConsumer,
    classifier: ClassificationWorker,
    applications: ClassificationApplicationWorker,
    backfills: ClassificationBackfillWorker,
}

impl ClassificationRuntime {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        pool: PgPool,
        ledger: LedgerFacade,
        banking: BankingFacade,
        categories: CategoryCatalogFacade,
        automation: ClassificationAutomationFacade,
        classifier: Arc<dyn TransactionClassifier>,
        auto_apply_enabled: bool,
    ) -> anyhow::Result<Self> {
        let store = automation.store();
        let evidence = ClassificationEvidenceBuilder {
            ledger: ledger.clone(),
            banking: banking.clone(),
            categories: categories.clone(),
            automation: automation.clone(),
        };
        Ok(Self {
            intake: ClassificationIntakeConsumer {
                pool,
                ledger: ledger.clone(),
                banking,
                automation,
                evidence: evidence.clone(),
            },
            classifier: ClassificationWorker::new(
                store.clone(),
                classifier,
                "moneykeeper-classifier",
                Duration::from_secs(45),
                100,
                5,
                crate::contexts::classification::public::ThresholdPolicy::CONSERVATIVE,
                auto_apply_enabled,
            )?,
            applications: ClassificationApplicationWorker {
                store: store.clone(),
                ledger,
                categories,
                evidence: evidence.clone(),
                holder: "moneykeeper-classification-application".to_owned(),
                lease_ttl: Duration::from_secs(30),
                auto_apply_enabled,
            },
            backfills: ClassificationBackfillWorker {
                store,
                ledger: evidence.ledger.clone(),
                evidence,
                holder: "moneykeeper-classification-backfill".to_owned(),
                lease_ttl: Duration::from_secs(30),
            },
        })
    }

    pub(crate) async fn run_intake_once(&self) -> anyhow::Result<ClassificationRunReport> {
        self.intake.run_once().await
    }

    pub(crate) async fn run_classifier_once(&self) -> anyhow::Result<ClassificationRunReport> {
        let report = self.classifier.run_once().await?;
        Ok(ClassificationRunReport {
            claimed: report.claimed,
            records: u32::from(report.predicted),
            retry_scheduled: report.retry_scheduled || report.quota_deferred,
            fenced: report.fenced,
            failed: report.failed,
        })
    }

    pub(crate) async fn run_application_once(&self) -> anyhow::Result<ClassificationRunReport> {
        self.applications.run_once().await
    }

    pub(crate) async fn run_backfill_once(&self) -> anyhow::Result<ClassificationRunReport> {
        self.backfills.run_once().await
    }
}

#[derive(Clone)]
struct ClassificationIntakeConsumer {
    pool: PgPool,
    ledger: LedgerFacade,
    banking: BankingFacade,
    automation: ClassificationAutomationFacade,
    evidence: ClassificationEvidenceBuilder,
}

impl ClassificationIntakeConsumer {
    async fn run_once(&self) -> anyhow::Result<ClassificationRunReport> {
        let Some(event) = next_event(&self.pool).await? else {
            return self.refresh_one_stale().await;
        };
        let queued = self.consume(&event).await?;
        let digest = Sha256::digest(serde_json::to_vec(&event.payload)?);
        self.automation
            .record_consumed_event(
                CONSUMER_NAME,
                event.event_id,
                &event.event_type,
                event.sequence,
                digest.as_slice(),
            )
            .await?;
        acknowledge(&self.pool, &event).await?;
        let refreshed = self.refresh_one_stale().await?;
        Ok(ClassificationRunReport {
            claimed: true,
            records: u32::from(queued) + refreshed.records,
            ..ClassificationRunReport::default()
        })
    }

    async fn refresh_one_stale(&self) -> anyhow::Result<ClassificationRunReport> {
        let Some(target) = self.automation.store().next_stale_target().await? else {
            return Ok(ClassificationRunReport::default());
        };
        let journal = self
            .ledger
            .get_journal(target.user_id, JournalEntryId::new(target.journal_entry_id))
            .await?;
        let queued = self.refresh_stale_target(target, &journal).await?;
        Ok(ClassificationRunReport {
            claimed: true,
            records: u32::from(queued),
            fenced: !queued,
            ..ClassificationRunReport::default()
        })
    }

    async fn refresh_stale_target(
        &self,
        target: StaleTarget,
        journal: &JournalView,
    ) -> anyhow::Result<bool> {
        let evidence = self
            .evidence
            .build(target.user_id, journal, None, EvidenceUse::Target)
            .await?;
        let Some(evidence) = evidence else {
            self.automation
                .store()
                .retire_stale_target(target, Utc::now())
                .await?;
            return Ok(false);
        };
        Ok(self
            .automation
            .enqueue(&evidence, target.origin, target.backfill_job_id)
            .await?
            .queued)
    }

    async fn consume(&self, event: &ClassificationEvent) -> anyhow::Result<bool> {
        if event.schema_version != 1 && is_classification_event(&event.event_type) {
            anyhow::bail!("unsupported classification event schema");
        }
        let user_id = UserId::new(event.user_id);
        match event.event_type.as_str() {
            JOURNAL_REVERSED_V1 | JOURNAL_REPLACED_V1 => {
                let journal_id = Uuid::parse_str(&event.aggregate_id)
                    .context("classification journal event id is invalid")?;
                let journal = self
                    .ledger
                    .get_journal(user_id, JournalEntryId::new(journal_id))
                    .await?;
                if let Some(original) = journal
                    .relations
                    .reverses()
                    .or(journal.relations.replaces())
                {
                    self.automation
                        .fence_transaction(user_id, original.into_uuid(), None, Utc::now())
                        .await?;
                }
                Ok(false)
            }
            JOURNAL_POSTED_V1 => {
                let journal_id = Uuid::parse_str(&event.aggregate_id)
                    .context("classification journal event id is invalid")?;
                let journal = self
                    .ledger
                    .get_journal(user_id, JournalEntryId::new(journal_id))
                    .await?;
                if journal.source != JournalSource::Manual {
                    return Ok(false);
                }
                self.record_manual_feedback(user_id, &journal, &serde_json::json!({}))
                    .await?;
                self.enqueue_if_eligible(user_id, journal, None).await
            }
            PROVIDER_TRANSACTION_IMPORTED_V1 => {
                let imported: ProviderTransactionImportedV1 =
                    serde_json::from_value(event.payload.clone())
                        .context("classification provider event payload is invalid")?;
                let provider = self
                    .banking
                    .classification_evidence(user_id, imported.provider_event_id)
                    .await?;
                if provider.journal_entry_id != imported.journal_entry_id {
                    anyhow::bail!("classification provider evidence is inconsistent");
                }
                let journal = self
                    .ledger
                    .get_journal(user_id, imported.journal_entry_id)
                    .await?;
                self.enqueue_if_eligible(user_id, journal, Some(provider))
                    .await
            }
            ANNOTATION_CHANGED_V1 | CATEGORY_ASSIGNMENT_CHANGED_V1 => {
                let journal_id = payload_uuid(&event.payload, "journal_entry_id")?;
                let journal = self
                    .ledger
                    .get_journal(user_id, JournalEntryId::new(journal_id))
                    .await?;
                let queued = self
                    .enqueue_if_eligible(user_id, journal.clone(), None)
                    .await?;
                if journal.annotation.as_ref().is_none_or(|annotation| {
                    annotation.category_id.is_some()
                        || annotation.automation_state != AutomationState::Eligible
                }) {
                    self.automation
                        .fence_transaction(
                            user_id,
                            journal_id,
                            journal
                                .annotation
                                .as_ref()
                                .and_then(|annotation| annotation.classification_decision_id),
                            Utc::now(),
                        )
                        .await?;
                }
                if event.event_type == CATEGORY_ASSIGNMENT_CHANGED_V1 {
                    self.record_manual_feedback(user_id, &journal, &event.payload)
                        .await?;
                }
                Ok(queued)
            }
            _ => Ok(false),
        }
    }

    async fn enqueue_if_eligible(
        &self,
        user_id: UserId,
        journal: JournalView,
        provider: Option<ProviderClassificationEvidence>,
    ) -> anyhow::Result<bool> {
        let Some(evidence) = self
            .evidence
            .build(user_id, &journal, provider, EvidenceUse::Target)
            .await?
        else {
            return Ok(false);
        };
        Ok(self
            .automation
            .enqueue(&evidence, TargetOrigin::Live, None)
            .await?
            .queued)
    }

    async fn record_manual_feedback(
        &self,
        user_id: UserId,
        journal: &JournalView,
        payload: &Value,
    ) -> anyhow::Result<()> {
        let Some(annotation) = journal.annotation.as_ref() else {
            return Ok(());
        };
        if annotation.assignment_origin != Some(AssignmentOrigin::Manual) {
            return Ok(());
        }
        // Review resolutions are persisted as manual intent in Ledger, but their
        // accepted/corrected/rejected examples are recorded transactionally when
        // the decision finishes. Recording the resulting Ledger event again would
        // double-weight the same user feedback.
        if annotation.classification_decision_id.is_some() {
            return Ok(());
        }
        let positive = annotation.category_id.map(|id| id.into_uuid());
        let previous_origin = payload
            .get("previous_assignment_origin")
            .and_then(Value::as_str);
        let previous_category = payload_uuid_optional(payload, "previous_category_id")?;
        let negative = if matches!(previous_origin, Some("ai" | "recurring"))
            && previous_category != positive
        {
            previous_category
        } else {
            None
        };
        if positive.is_none() && negative.is_none() {
            return Ok(());
        }
        let Some(evidence) = self
            .evidence
            .build(user_id, journal, None, EvidenceUse::Feedback)
            .await?
        else {
            return Ok(());
        };
        self.automation
            .record_manual_feedback(&evidence, positive, negative, Utc::now())
            .await?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EvidenceUse {
    Target,
    Feedback,
}

#[derive(Clone)]
struct ClassificationEvidenceBuilder {
    ledger: LedgerFacade,
    banking: BankingFacade,
    categories: CategoryCatalogFacade,
    automation: ClassificationAutomationFacade,
}

impl ClassificationEvidenceBuilder {
    async fn build(
        &self,
        user_id: UserId,
        journal: &JournalView,
        provider_hint: Option<ProviderClassificationEvidence>,
        usage: EvidenceUse,
    ) -> anyhow::Result<Option<ClassificationEvidence>> {
        if journal.purpose != PostingPurpose::Ordinary
            || journal.reversed_by_journal_id.is_some()
            || journal.replaced_by_journal_id.is_some()
            || !matches!(
                journal.source,
                JournalSource::Manual | JournalSource::Import
            )
        {
            return Ok(None);
        }
        let Some(annotation) = journal.annotation.as_ref() else {
            return Ok(None);
        };
        if usage == EvidenceUse::Target
            && (annotation.category_id.is_some()
                || annotation.automation_state != AutomationState::Eligible)
        {
            return Ok(None);
        }
        let Some(cash_flow_kind) = cash_flow_kind(journal) else {
            return Ok(None);
        };
        let Some(account_posting) = journal
            .postings
            .iter()
            .find(|posting| posting.account_kind != AccountKind::System)
        else {
            return Ok(None);
        };
        let provider = match provider_hint {
            Some(value) => Some(value),
            None if journal.source == JournalSource::Import => {
                self.banking
                    .classification_evidence_for_journal(user_id, journal.id)
                    .await?
            }
            None => None,
        };
        if provider
            .as_ref()
            .is_some_and(|value| value.journal_entry_id != journal.id)
        {
            anyhow::bail!("classification evidence journal mismatch");
        }
        let taxonomy = self.categories.taxonomy(user_id, Utc::now()).await?;
        let mut choices = Vec::new();
        collect_categories(&taxonomy.roots, cash_flow_kind, &mut choices)?;
        if choices.is_empty() {
            return Ok(None);
        }
        let allowed: HashSet<Uuid> = choices.iter().map(ClassificationCategory::id).collect();
        let merchant_mcc = provider
            .as_ref()
            .and_then(|value| value.merchant_mcc)
            .and_then(|value| u16::try_from(value).ok());
        let provider_name = provider.as_ref().map(|value| value.provider.clone());
        let mut examples = self
            .automation
            .ranked_examples(
                user_id,
                provider_name.as_deref(),
                merchant_mcc,
                MAX_EXAMPLES,
            )
            .await?;
        examples.retain(|example| match example.signal() {
            FeedbackSignal::Positive { category_id } | FeedbackSignal::Negative { category_id } => {
                allowed.contains(&category_id)
            }
        });
        let account = self
            .ledger
            .get_account(user_id, account_posting.account_id)
            .await?;
        let (description, amount, currency, occurred_at) = provider.as_ref().map_or_else(
            || {
                (
                    annotation.description.clone(),
                    account_posting.display_effect.abs(),
                    account_posting.currency.to_string(),
                    journal.occurred_at,
                )
            },
            |value| {
                (
                    value.description.clone(),
                    value.operation_money.amount().abs(),
                    value.operation_money.currency().to_string(),
                    value.effective_at,
                )
            },
        );
        let evidence = ClassificationEvidence::new(ClassificationEvidenceInput {
            user_id,
            journal_entry_id: journal.id.into_uuid(),
            description,
            amount,
            currency,
            occurred_at,
            cash_flow_kind,
            provider: provider_name,
            merchant_mcc,
            account_label: Some(account.name),
            taxonomy_version: taxonomy.version,
            annotation_version: annotation.version.get(),
            categories: choices,
            examples,
        })?;
        Ok(Some(evidence))
    }
}

fn collect_categories(
    nodes: &[CategoryNodeView],
    cash_flow_kind: crate::contexts::classification::public::CashFlowKind,
    output: &mut Vec<ClassificationCategory>,
) -> anyhow::Result<()> {
    for node in nodes {
        let category = &node.category;
        let compatible = matches!(
            (category.kind, cash_flow_kind),
            (CategoryKind::Both, _)
                | (
                    CategoryKind::Income,
                    crate::contexts::classification::public::CashFlowKind::Income,
                )
                | (
                    CategoryKind::Expense,
                    crate::contexts::classification::public::CashFlowKind::Expense,
                )
        );
        if category.assignable && compatible {
            let kind = match category.kind {
                CategoryKind::Income => ClassificationCategoryKind::Income,
                CategoryKind::Expense => ClassificationCategoryKind::Expense,
                CategoryKind::Both => ClassificationCategoryKind::Both,
            };
            output.push(ClassificationCategory::new(
                category.id.into_uuid(),
                category.path.join(" / "),
                kind,
            )?);
        }
        collect_categories(&node.children, cash_flow_kind, output)?;
    }
    Ok(())
}

fn cash_flow_kind(
    journal: &JournalView,
) -> Option<crate::contexts::classification::public::CashFlowKind> {
    let mut kind = None;
    for posting in &journal.postings {
        let candidate = match posting.account_nature {
            AccountNature::Income => {
                Some(crate::contexts::classification::public::CashFlowKind::Income)
            }
            AccountNature::Expense => {
                Some(crate::contexts::classification::public::CashFlowKind::Expense)
            }
            _ => None,
        };
        if let Some(candidate) = candidate {
            if kind.is_some_and(|current| current != candidate) {
                return None;
            }
            kind = Some(candidate);
        }
    }
    kind
}

#[derive(Clone)]
struct ClassificationApplicationWorker {
    store: ClassificationWorkFacade,
    ledger: LedgerFacade,
    categories: CategoryCatalogFacade,
    evidence: ClassificationEvidenceBuilder,
    holder: String,
    lease_ttl: Duration,
    auto_apply_enabled: bool,
}

impl ClassificationApplicationWorker {
    async fn run_once(&self) -> anyhow::Result<ClassificationRunReport> {
        let Some(claim) = self
            .store
            .claim_application(&self.holder, self.lease_ttl)
            .await?
        else {
            return Ok(ClassificationRunReport::default());
        };
        let journal = self
            .ledger
            .get_journal(claim.user_id, JournalEntryId::new(claim.journal_entry_id))
            .await?;
        // Keep taxonomy mutation behind this assignment and its completion receipt.
        // A stale version is rejected before Ledger can write anything.
        let taxonomy = self.categories.assignment_guard(claim.user_id).await?;
        let expected_origin = if claim.action.is_some() {
            AssignmentOrigin::Manual
        } else {
            AssignmentOrigin::Ai
        };
        let already_applied = journal.annotation.as_ref().is_some_and(|annotation| {
            annotation.assignment_origin == Some(expected_origin)
                && annotation.classification_decision_id == Some(claim.decision_id.into_uuid())
                && annotation.category_id.map(|id| id.into_uuid()) == claim.category_id
        });
        if already_applied {
            self.store
                .complete_application(&claim, &self.holder, Utc::now())
                .await?;
            return Ok(ClassificationRunReport {
                claimed: true,
                records: 1,
                ..ClassificationRunReport::default()
            });
        }
        let fresh = journal.annotation.as_ref().is_some_and(|annotation| {
            annotation.version.get() == claim.annotation_version
                && annotation.category_id.is_none()
                && annotation.automation_state == AutomationState::Eligible
        }) && taxonomy.version() == claim.taxonomy_version
            && journal.reversed_by_journal_id.is_none()
            && journal.replaced_by_journal_id.is_none();
        if !fresh {
            self.stale_and_refresh(&claim, &journal).await?;
            return Ok(ClassificationRunReport {
                claimed: true,
                fenced: true,
                ..ClassificationRunReport::default()
            });
        }
        if claim.action.is_none() && !self.auto_apply_enabled {
            self.store
                .return_application_to_review(&claim, &self.holder, Utc::now())
                .await?;
            return Ok(ClassificationRunReport {
                claimed: true,
                records: 1,
                ..ClassificationRunReport::default()
            });
        }
        let expected_version = AnnotationVersion::new(claim.annotation_version)?;
        let key = IdempotencyKey::new(format!("classification-apply-{}", claim.decision_id))?;
        let correlation_id = CorrelationId::new(claim.decision_id.into_uuid());
        let applied = match claim.action {
            None if claim.category_id.is_none() => {
                self.store
                    .mark_application_failed(&claim, &self.holder, Utc::now())
                    .await?;
                return Ok(ClassificationRunReport {
                    claimed: true,
                    failed: true,
                    ..ClassificationRunReport::default()
                });
            }
            action => self
                .ledger
                .apply_category_assignment(ApplyCategoryAssignment {
                    user_id: claim.user_id,
                    journal_entry_id: JournalEntryId::new(claim.journal_entry_id),
                    category_id: claim
                        .category_id
                        .map(crate::contexts::classification::public::CategoryId::new),
                    origin: if action.is_some() {
                        AssignmentOrigin::Manual
                    } else {
                        AssignmentOrigin::Ai
                    },
                    classification_decision_id: Some(claim.decision_id.into_uuid()),
                    expected_version,
                    idempotency_key: key,
                    correlation_id,
                    occurred_at: claim.occurred_at,
                })
                .await
                .map(|_| ()),
        };
        match applied {
            Ok(()) => {
                self.store
                    .complete_application(&claim, &self.holder, Utc::now())
                    .await?;
                Ok(ClassificationRunReport {
                    claimed: true,
                    records: 1,
                    ..ClassificationRunReport::default()
                })
            }
            Err(error) if is_stale_ledger_error(&error) => {
                let current = self
                    .ledger
                    .get_journal(claim.user_id, JournalEntryId::new(claim.journal_entry_id))
                    .await?;
                self.stale_and_refresh(&claim, &current).await?;
                Ok(ClassificationRunReport {
                    claimed: true,
                    fenced: true,
                    ..ClassificationRunReport::default()
                })
            }
            Err(_error) => {
                self.store
                    .mark_application_failed(&claim, &self.holder, Utc::now())
                    .await?;
                Ok(ClassificationRunReport {
                    claimed: true,
                    failed: true,
                    ..ClassificationRunReport::default()
                })
            }
        }
    }

    async fn stale_and_refresh(
        &self,
        claim: &ApplicationClaim,
        journal: &JournalView,
    ) -> anyhow::Result<()> {
        self.store
            .mark_application_stale(claim, &self.holder, Utc::now())
            .await?;
        if let Some(evidence) = self
            .evidence
            .build(claim.user_id, journal, None, EvidenceUse::Target)
            .await?
        {
            self.evidence
                .automation
                .enqueue(&evidence, TargetOrigin::Live, None)
                .await?;
        }
        Ok(())
    }
}

fn is_stale_ledger_error(error: &LedgerError) -> bool {
    error.is_version_conflict()
        || error.is_invalid_annotation()
        || error.is_not_found()
        || error.is_tenant_mismatch()
}

#[derive(Clone)]
struct ClassificationBackfillWorker {
    store: ClassificationWorkFacade,
    ledger: LedgerFacade,
    evidence: ClassificationEvidenceBuilder,
    holder: String,
    lease_ttl: Duration,
}

impl ClassificationBackfillWorker {
    async fn run_once(&self) -> anyhow::Result<ClassificationRunReport> {
        let Some(claim) = self
            .store
            .claim_backfill(&self.holder, self.lease_ttl)
            .await?
        else {
            return Ok(ClassificationRunReport::default());
        };
        self.process(claim).await
    }

    async fn process(&self, claim: BackfillClaim) -> anyhow::Result<ClassificationRunReport> {
        let filter = ActivityFilter::new(claim.range.from(), claim.range.to(), ActivityKind::All)?
            .with_uncategorized()?;
        let after = claim
            .cursor_occurred_at
            .zip(claim.cursor_ledger_sequence)
            .map(|(occurred_at, ledger_sequence)| ActivityCursor {
                occurred_at,
                ledger_sequence,
            });
        let journals = self
            .ledger
            .list_activity(claim.user_id, filter, after, 1)
            .await?;
        let Some(mut journal) = journals.into_iter().next() else {
            self.store
                .advance_backfill(
                    &claim,
                    &self.holder,
                    claim.cursor_occurred_at,
                    claim.cursor_journal_entry_id,
                    claim.cursor_ledger_sequence,
                    true,
                    Utc::now(),
                )
                .await?;
            return Ok(ClassificationRunReport {
                claimed: true,
                ..ClassificationRunReport::default()
            });
        };
        // Only an explicit historical job may opt legacy ambiguous clears in.
        // The annotation version check protects a concurrent manual edit.
        if journal.purpose == PostingPurpose::Ordinary
            && journal.reversed_by_journal_id.is_none()
            && journal.replaced_by_journal_id.is_none()
            && matches!(
                journal.source,
                JournalSource::Manual | JournalSource::Import
            )
            && cash_flow_kind(&journal).is_some()
            && let Some(annotation) = journal.annotation.as_ref()
            && annotation.automation_state == AutomationState::LegacyUnknown
        {
            let result = self
                .ledger
                .enable_automatic_classification(
                    crate::contexts::ledger::public::EnableAutomaticClassification {
                        user_id: claim.user_id,
                        journal_entry_id: journal.id,
                        expected_version: annotation.version,
                        idempotency_key: IdempotencyKey::new(format!(
                            "backfill-enable-{}-{}",
                            claim.id, journal.id
                        ))?,
                        correlation_id: CorrelationId::new(claim.id.into_uuid()),
                        occurred_at: claim.range.from(),
                    },
                )
                .await;
            match result {
                Ok(_) => journal = self.ledger.get_journal(claim.user_id, journal.id).await?,
                Err(error) if is_stale_ledger_error(&error) => {}
                Err(error) => return Err(error.into()),
            }
        }
        let queued = if let Some(evidence) = self
            .evidence
            .build(claim.user_id, &journal, None, EvidenceUse::Target)
            .await?
        {
            self.evidence
                .automation
                .enqueue(&evidence, TargetOrigin::Backfill, Some(claim.id))
                .await?
                .queued
        } else {
            false
        };
        self.store
            .advance_backfill(
                &claim,
                &self.holder,
                Some(journal.occurred_at),
                Some(journal.id.into_uuid()),
                Some(journal.ledger_sequence),
                false,
                Utc::now(),
            )
            .await?;
        Ok(ClassificationRunReport {
            claimed: true,
            records: u32::from(queued),
            ..ClassificationRunReport::default()
        })
    }
}

struct ClassificationEvent {
    event_id: Uuid,
    sequence: i64,
    schema_version: i32,
    aggregate_id: String,
    event_type: String,
    user_id: Uuid,
    payload: Value,
}

async fn next_event(pool: &PgPool) -> Result<Option<ClassificationEvent>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT o.sequence,o.event_id,o.message_schema_version,o.aggregate_id,o.event_type,o.user_id,o.payload \
         FROM integration.outbox_messages o WHERE o.event_type IN (\
           'ledger.journal-posted.v1','ledger.journal-reversed.v1',\
           'ledger.journal-replaced.v1','ledger.annotation-changed.v1',\
           'ledger.category-assignment-changed.v1',\
           'banking.provider-transaction-imported.v1'\
         ) AND NOT EXISTS( \
           SELECT 1 FROM integration.inbox_receipts i WHERE i.consumer_name=$1 AND i.message_id=o.event_id \
         ) ORDER BY o.sequence,o.event_id LIMIT 1",
    )
    .bind(CONSUMER_NAME)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|row| ClassificationEvent {
        event_id: row.get("event_id"),
        sequence: row.get("sequence"),
        schema_version: row.get("message_schema_version"),
        aggregate_id: row.get("aggregate_id"),
        event_type: row.get("event_type"),
        user_id: row.get("user_id"),
        payload: row.get("payload"),
    }))
}

async fn acknowledge(pool: &PgPool, event: &ClassificationEvent) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO integration.inbox_receipts(consumer_name,message_id,event_type,received_at,processed_at) \
         VALUES($1,$2,$3,clock_timestamp(),clock_timestamp()) \
         ON CONFLICT(consumer_name,message_id) DO UPDATE SET processed_at=EXCLUDED.processed_at",
    )
    .bind(CONSUMER_NAME)
    .bind(event.event_id)
    .bind(&event.event_type)
    .execute(pool)
    .await?;
    Ok(())
}

fn is_classification_event(event_type: &str) -> bool {
    matches!(
        event_type,
        JOURNAL_POSTED_V1
            | JOURNAL_REVERSED_V1
            | JOURNAL_REPLACED_V1
            | PROVIDER_TRANSACTION_IMPORTED_V1
            | ANNOTATION_CHANGED_V1
            | CATEGORY_ASSIGNMENT_CHANGED_V1
    )
}

fn payload_uuid(payload: &Value, key: &str) -> anyhow::Result<Uuid> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .context("classification event UUID is missing")?
        .parse()
        .context("classification event UUID is invalid")
}

fn payload_uuid_optional(payload: &Value, key: &str) -> anyhow::Result<Option<Uuid>> {
    payload
        .get(key)
        .filter(|value| !value.is_null())
        .map(|value| {
            value
                .as_str()
                .context("classification event UUID is invalid")?
                .parse()
                .context("classification event UUID is invalid")
        })
        .transpose()
}
