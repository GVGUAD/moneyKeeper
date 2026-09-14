//! Moneykeeper composition root and application lifecycle.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Context;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine as _;
use jsonwebtoken::jwk::JwkSet;
use rand::RngCore as _;
use serde_json::json;

use crate::bootstrap::workers::{Readiness, WorkerRegistry};
use crate::contexts::banking::{
    adapters::{Aes256CredentialCipher, MonobankClient},
    public::BankingFacade,
};
use crate::contexts::classification::public::{
    CategoryCatalogFacade, ClassificationAutomationFacade,
};
use crate::contexts::ledger::public::LedgerFacade;
use crate::contexts::loans::public::LoansFacade;
use crate::contexts::mail::public::MailFacade;
use crate::contexts::portfolio::public::PortfolioFacade;
use crate::contexts::preferences::public::PreferencesFacade;
use crate::contexts::recurring::public::RecurringFacade;
use crate::contexts::reference_data::public::CurrencyCatalogFacade;
use crate::contexts::reporting::public::ReportingFacade;
use crate::contexts::sharing::public::SharingFacade;
use crate::infrastructure::database::VerifiedDatabase;

/// Stable production secrets required to build Banking adapters.
#[derive(Clone)]
pub struct RuntimeSecrets {
    banking_key_id: String,
    banking_key: [u8; 32],
    webhook_digest_key: [u8; 32],
    openai_api_key: String,
    classification_model: String,
    classification_auto_apply: bool,
}

impl std::fmt::Debug for RuntimeSecrets {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeSecrets")
            .field("banking_key_id", &self.banking_key_id)
            .field("banking_key", &"[REDACTED]")
            .field("webhook_digest_key", &"[REDACTED]")
            .field("openai_api_key", &"[REDACTED]")
            .field("classification_model", &self.classification_model)
            .field("classification_auto_apply", &self.classification_auto_apply)
            .finish()
    }
}

impl RuntimeSecrets {
    /// Loads and validates the stable Moneykeeper cryptographic configuration.
    pub fn from_environment() -> anyhow::Result<Self> {
        let banking_key_id = required_environment("FINANCE_V2_ENCRYPTION_KEY_ID")?;
        anyhow::ensure!(
            banking_key_id.len() <= 100
                && banking_key_id.trim() == banking_key_id
                && !banking_key_id.chars().any(char::is_control),
            "FINANCE_V2_ENCRYPTION_KEY_ID is invalid"
        );
        let classification_model = std::env::var("CLASSIFICATION_MODEL").unwrap_or_else(|_| {
            crate::contexts::classification::automation::DEFAULT_OPENAI_MODEL.to_owned()
        });
        anyhow::ensure!(
            classification_model.trim() == classification_model
                && !classification_model.is_empty()
                && classification_model.len() <= 200
                && !classification_model.chars().any(char::is_control),
            "CLASSIFICATION_MODEL is invalid"
        );
        let classification_auto_apply = match std::env::var("CLASSIFICATION_AUTO_APPLY") {
            Ok(value) if value.eq_ignore_ascii_case("true") => true,
            Ok(value) if value.eq_ignore_ascii_case("false") => false,
            Ok(_) => anyhow::bail!("CLASSIFICATION_AUTO_APPLY must be true or false"),
            Err(std::env::VarError::NotPresent) => false,
            Err(error) => return Err(error).context("read CLASSIFICATION_AUTO_APPLY"),
        };
        Ok(Self {
            banking_key_id,
            banking_key: decode_key("FINANCE_V2_ENCRYPTION_KEY")?,
            webhook_digest_key: decode_key("FINANCE_V2_WEBHOOK_DIGEST_KEY")?,
            openai_api_key: required_environment("OPENAI_API_KEY")?,
            classification_model,
            classification_auto_apply,
        })
    }

    fn ephemeral() -> Self {
        let mut banking_key = [0_u8; 32];
        let mut webhook_digest_key = [0_u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut banking_key);
        rand::rngs::OsRng.fill_bytes(&mut webhook_digest_key);
        Self {
            banking_key_id: "ephemeral-moneykeeper".to_owned(),
            banking_key,
            webhook_digest_key,
            openai_api_key: "ephemeral-openai-key".to_owned(),
            classification_model: crate::contexts::classification::automation::DEFAULT_OPENAI_MODEL
                .to_owned(),
            classification_auto_apply: false,
        }
    }

    pub(crate) fn openai_api_key(&self) -> &str {
        &self.openai_api_key
    }

    pub(crate) fn classification_model(&self) -> &str {
        &self.classification_model
    }

    pub(crate) const fn classification_auto_apply(&self) -> bool {
        self.classification_auto_apply
    }
}

fn required_environment(name: &str) -> anyhow::Result<String> {
    let value = std::env::var(name).with_context(|| format!("{name} must be set"))?;
    anyhow::ensure!(!value.trim().is_empty(), "{name} must not be empty");
    Ok(value)
}

fn decode_key(name: &str) -> anyhow::Result<[u8; 32]> {
    let encoded = required_environment(name)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .with_context(|| format!("{name} must be valid base64"))?;
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("{name} must decode to exactly 32 bytes"))
}

/// Validated process configuration. Debug output intentionally omits secrets
/// and the database URL.
pub struct RuntimeConfig {
    database_url: String,
    bind_address: std::net::SocketAddr,
    supabase_url: reqwest::Url,
    monobank_webhook_base_url: reqwest::Url,
    secrets: RuntimeSecrets,
}

impl std::fmt::Debug for RuntimeConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeConfig")
            .field("database_url", &"[REDACTED]")
            .field("bind_address", &self.bind_address)
            .field("supabase_url", &self.supabase_url)
            .field("monobank_webhook_base_url", &self.monobank_webhook_base_url)
            .field("secrets", &self.secrets)
            .finish()
    }
}

impl RuntimeConfig {
    /// Loads all startup-critical values before database or provider work.
    pub fn from_environment() -> anyhow::Result<Self> {
        let database_url = required_environment("DATABASE_URL")?;
        let bind_address = std::env::var("BIND_ADDR")
            .unwrap_or_else(|_| "0.0.0.0:8080".to_owned())
            .parse()
            .context("BIND_ADDR must be a socket address")?;
        let supabase_url = reqwest::Url::parse(&required_environment("SUPABASE_URL")?)
            .context("SUPABASE_URL must be an absolute URL")?;
        anyhow::ensure!(
            matches!(supabase_url.scheme(), "http" | "https"),
            "SUPABASE_URL must use HTTP or HTTPS"
        );
        let monobank_webhook_base_url = validate_monobank_webhook_base_url(&required_environment(
            "MONOBANK_WEBHOOK_BASE_URL",
        )?)?;
        for name in [
            "GMAIL_CLIENT_ID",
            "GMAIL_CLIENT_SECRET",
            "GMAIL_REDIRECT_URI",
        ] {
            required_environment(name)?;
        }
        Ok(Self {
            database_url,
            bind_address,
            supabase_url,
            monobank_webhook_base_url,
            secrets: RuntimeSecrets::from_environment()?,
        })
    }

    pub fn database_url(&self) -> &str {
        &self.database_url
    }

    pub fn bind_address(&self) -> std::net::SocketAddr {
        self.bind_address
    }

    pub fn jwks_url(&self) -> reqwest::Url {
        self.supabase_url
            .join("auth/v1/.well-known/jwks.json")
            .expect("validated base URL accepts the static JWKS path")
    }

    pub fn secrets(&self) -> &RuntimeSecrets {
        &self.secrets
    }

    pub fn monobank_webhook_base_url(&self) -> &reqwest::Url {
        &self.monobank_webhook_base_url
    }
}

fn validate_monobank_webhook_base_url(value: &str) -> anyhow::Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(value)
        .context("MONOBANK_WEBHOOK_BASE_URL must be an absolute HTTPS URL")?;
    anyhow::ensure!(
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.host_str().is_some(),
        "MONOBANK_WEBHOOK_BASE_URL must be absolute HTTPS without credentials, query, or fragment"
    );
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

#[cfg(test)]
mod webhook_base_url_tests {
    use super::validate_monobank_webhook_base_url;

    #[test]
    fn accepts_only_secret_safe_https_bases() {
        assert_eq!(
            validate_monobank_webhook_base_url("https://moneykeeper.example/api")
                .unwrap()
                .as_str(),
            "https://moneykeeper.example/api/"
        );
        for invalid in [
            "http://moneykeeper.example/",
            "https://user@moneykeeper.example/",
            "https://moneykeeper.example/?secret=value",
            "https://moneykeeper.example/#fragment",
            "/relative",
        ] {
            assert!(
                validate_monobank_webhook_base_url(invalid).is_err(),
                "{invalid}"
            );
        }
    }
}

/// Public context capabilities assembled only after database lineage
/// verification. Concrete PostgreSQL adapters remain context-private.
#[derive(Clone)]
pub struct ContextFacades {
    pub currencies: CurrencyCatalogFacade,
    pub categories: CategoryCatalogFacade,
    pub classification: ClassificationAutomationFacade,
    pub preferences: PreferencesFacade,
    pub ledger: LedgerFacade,
    pub banking: BankingFacade,
    pub mail: MailFacade,
    pub recurring: RecurringFacade,
    pub reporting: ReportingFacade,
    pub loans: LoansFacade,
    pub sharing: SharingFacade,
    pub portfolio: PortfolioFacade,
}

/// Builds all public context capabilities from a verified database.
pub fn build_contexts(pool: &VerifiedDatabase) -> ContextFacades {
    build_contexts_with_secrets(pool, &RuntimeSecrets::ephemeral())
}

/// Builds all context façades with stable production cryptographic material.
pub fn build_contexts_with_secrets(
    pool: &VerifiedDatabase,
    secrets: &RuntimeSecrets,
) -> ContextFacades {
    let categories = crate::contexts::classification::build(pool);
    let classification = crate::contexts::classification::build_automation(pool);
    let currencies = crate::contexts::reference_data::build(pool);
    let ledger = crate::contexts::ledger::build_with_categories(pool, categories.clone());
    let banking = crate::contexts::banking::build_with_ledger(
        pool,
        Arc::new(
            Aes256CredentialCipher::new(&secrets.banking_key_id, secrets.banking_key)
                .expect("validated Moneykeeper key has the required length"),
        ),
        Arc::new(MonobankClient::new("https://api.monobank.ua")),
        ledger.clone(),
        currencies.clone(),
        secrets.webhook_digest_key,
    );
    ContextFacades {
        currencies,
        categories: categories.clone(),
        classification,
        preferences: crate::contexts::preferences::build(pool),
        ledger,
        banking,
        mail: crate::contexts::mail::build_with_key(
            pool,
            &secrets.banking_key_id,
            secrets.banking_key,
        ),
        recurring: crate::contexts::recurring::build(pool),
        reporting: crate::contexts::reporting::build(pool),
        loans: crate::contexts::loans::build(pool),
        sharing: crate::contexts::sharing::build(pool),
        portfolio: crate::contexts::portfolio::build(pool),
    }
}

/// Builds the Moneykeeper HTTP router without spawning background workers.
pub fn router(pool: &VerifiedDatabase, jwks: Arc<JwkSet>) -> Router {
    crate::api::router(build_contexts(pool), jwks)
}

/// Owning-context maintenance workers constructed by the composition root.
pub(crate) struct ContextMaintenanceWorkers {
    mail: crate::contexts::mail::infrastructure::sync_worker::MailSyncWorker<
        crate::contexts::mail::infrastructure::gmail::GmailClient,
        crate::contexts::mail::infrastructure::oauth::GoogleOAuthClient,
    >,
    fx: crate::contexts::reference_data::infrastructure::nbu::NbuSyncWorker<
        crate::contexts::reference_data::infrastructure::nbu::NbuClient,
    >,
    recurring:
        crate::contexts::recurring::infrastructure::categorization_worker::CategorizationWorker,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkerRunReport {
    pub claimed: bool,
    pub records: u32,
    pub replayed: u32,
    pub retry_scheduled: bool,
    pub fenced: bool,
    pub dead_lettered: u32,
}

impl WorkerRunReport {
    pub(crate) fn merge(&mut self, other: Self) {
        self.claimed |= other.claimed;
        self.records = self.records.saturating_add(other.records);
        self.replayed = self.replayed.saturating_add(other.replayed);
        self.retry_scheduled |= other.retry_scheduled;
        self.fenced |= other.fenced;
        self.dead_lettered = self.dead_lettered.saturating_add(other.dead_lettered);
    }
}

impl ContextMaintenanceWorkers {
    pub(crate) async fn run_mail_once(&self) -> anyhow::Result<WorkerRunReport> {
        let report = self.mail.run_once().await?;
        Ok(WorkerRunReport {
            claimed: report.claimed,
            records: report.messages_recorded,
            replayed: 0,
            retry_scheduled: report.retry_scheduled,
            fenced: report.fenced,
            dead_lettered: 0,
        })
    }

    pub(crate) async fn run_reference_data_once(&self) -> anyhow::Result<WorkerRunReport> {
        let report = self.fx.run_once().await?;
        Ok(WorkerRunReport {
            claimed: report.claimed,
            records: report.observations,
            replayed: report.replayed,
            retry_scheduled: report.retry_scheduled,
            fenced: report.fenced,
            dead_lettered: 0,
        })
    }

    pub(crate) async fn run_recurring_once(&self) -> anyhow::Result<WorkerRunReport> {
        let report = self.recurring.run_once().await?;
        Ok(WorkerRunReport {
            claimed: report.claimed,
            records: u32::from(report.posted || report.compensated),
            replayed: 0,
            retry_scheduled: report.retry_scheduled,
            fenced: report.fenced,
            dead_lettered: 0,
        })
    }
}

pub(crate) fn context_maintenance_workers(
    pool: &VerifiedDatabase,
    secrets: &RuntimeSecrets,
) -> ContextMaintenanceWorkers {
    let categories = crate::contexts::classification::build(pool);
    let ledger = crate::contexts::ledger::build_with_categories(pool, categories);
    ContextMaintenanceWorkers {
        mail: crate::contexts::mail::infrastructure::sync_worker::MailSyncWorker::new(
            pool.pool().clone(),
            crate::contexts::mail::infrastructure::gmail::GmailClient::new(
                "https://gmail.googleapis.com",
            ),
            crate::contexts::mail::infrastructure::oauth::GoogleOAuthClient::from_environment(),
            crate::contexts::mail::infrastructure::MailCrypto::new(
                &secrets.banking_key_id,
                secrets.banking_key,
            )
            .expect("validated Moneykeeper Mail key configuration"),
            "finance-v2-mail",
            Duration::from_secs(30),
        )
        .expect("static Mail worker configuration is valid"),
        fx: crate::contexts::reference_data::infrastructure::nbu::NbuSyncWorker::new(
            pool.pool().clone(),
            crate::contexts::reference_data::infrastructure::nbu::NbuClient::new(
                "https://bank.gov.ua",
            ),
            "finance-v2-nbu",
            Duration::from_secs(30),
            30,
        )
        .expect("static NBU worker configuration is valid"),
        recurring: crate::contexts::recurring::infrastructure::categorization_worker::CategorizationWorker::new(
            pool.pool().clone(),
            ledger.clone(),
            "finance-v2-recurring",
            Duration::from_secs(30),
        )
        .expect("static Recurring worker configuration is valid"),
    }
}

/// Independent durable event consumers for Recurring and Reporting.
pub struct EventConsumers {
    recurring: crate::integration::event_consumers::RecurringEventConsumer,
    reporting: crate::integration::event_consumers::ReportingEventConsumer,
}

impl EventConsumers {
    pub async fn run_recurring_once(&self) -> anyhow::Result<WorkerRunReport> {
        let report = self.recurring.run_once().await?;
        Ok(WorkerRunReport {
            claimed: report.claimed(),
            records: u32::from(report.applied),
            ..WorkerRunReport::default()
        })
    }

    pub async fn run_reporting_once(&self) -> anyhow::Result<WorkerRunReport> {
        let report = self.reporting.run_once().await?;
        Ok(WorkerRunReport {
            claimed: report.claimed(),
            records: u32::from(report.applied),
            ..WorkerRunReport::default()
        })
    }
}

pub fn event_consumers(pool: &VerifiedDatabase) -> EventConsumers {
    let categories = crate::contexts::classification::build(pool);
    let ledger = crate::contexts::ledger::build_with_categories(pool, categories);
    EventConsumers {
        recurring: crate::integration::event_consumers::RecurringEventConsumer::new(
            pool.pool().clone(),
            ledger.clone(),
            crate::contexts::recurring::build(pool),
        ),
        reporting: crate::integration::event_consumers::ReportingEventConsumer::new(
            pool.pool().clone(),
            ledger,
            crate::contexts::reporting::build(pool),
        ),
    }
}

/// Classification intake, provider, application, and historical workers.
pub(crate) fn classification_runtime(
    pool: &VerifiedDatabase,
    contexts: &ContextFacades,
    secrets: &RuntimeSecrets,
) -> anyhow::Result<crate::integration::classification::ClassificationRuntime> {
    let classifier = crate::contexts::classification::automation::OpenAiResponsesClassifier::new(
        secrets.openai_api_key().to_owned(),
        Some(secrets.classification_model().to_owned()),
    )?;
    crate::integration::classification::ClassificationRuntime::new(
        pool.pool().clone(),
        contexts.ledger.clone(),
        contexts.banking.clone(),
        contexts.categories.clone(),
        contexts.classification.clone(),
        Arc::new(classifier),
        secrets.classification_auto_apply(),
    )
}

/// Loan accounting process managers coordinated through public contracts.
pub struct LoanAccountingWorkers {
    opening: crate::integration::process_managers::loan_opening::LoanOpeningWorker,
    accounting: crate::integration::process_managers::loan_accounting::LoanAccountingWorker,
    reversal: crate::integration::process_managers::loan_reversal::LoanReversalWorker,
    replacement: crate::integration::process_managers::loan_replacement::LoanReplacementWorker,
}

impl LoanAccountingWorkers {
    pub async fn run_opening_once(&self) -> anyhow::Result<WorkerRunReport> {
        let report = self.opening.run_once().await?;
        Ok(WorkerRunReport {
            claimed: report.claimed,
            records: u32::from(report.posted),
            replayed: 0,
            retry_scheduled: report.retry_due,
            fenced: false,
            dead_lettered: 0,
        })
    }
    pub async fn run_accounting_once(&self) -> anyhow::Result<WorkerRunReport> {
        let report = self.accounting.run_once().await?;
        Ok(WorkerRunReport {
            claimed: report.claimed,
            records: u32::from(report.posted),
            replayed: 0,
            retry_scheduled: report.retry_due,
            fenced: false,
            dead_lettered: 0,
        })
    }
    pub async fn run_reversal_once(&self) -> anyhow::Result<WorkerRunReport> {
        let report = self.reversal.run_once().await?;
        Ok(WorkerRunReport {
            claimed: report.claimed,
            records: u32::from(report.posted),
            replayed: 0,
            retry_scheduled: report.retry_due,
            fenced: false,
            dead_lettered: 0,
        })
    }
    pub async fn run_replacement_once(&self) -> anyhow::Result<WorkerRunReport> {
        let report = self.replacement.run_once().await?;
        Ok(WorkerRunReport {
            claimed: report.claimed,
            records: u32::from(report.original_reversed),
            replayed: 0,
            retry_scheduled: report.retry_due,
            fenced: false,
            dead_lettered: 0,
        })
    }
}

pub fn loan_accounting_workers(pool: &VerifiedDatabase) -> LoanAccountingWorkers {
    let categories = crate::contexts::classification::build(pool);
    let ledger = crate::contexts::ledger::build_with_categories(pool, categories);
    let loans = crate::contexts::loans::build(pool);
    LoanAccountingWorkers {
        opening: crate::integration::process_managers::loan_opening::LoanOpeningWorker::new(
            loans.clone(),
            ledger.clone(),
        ),
        accounting:
            crate::integration::process_managers::loan_accounting::LoanAccountingWorker::new(
                loans.clone(),
                ledger.clone(),
            ),
        reversal: crate::integration::process_managers::loan_reversal::LoanReversalWorker::new(
            loans.clone(),
            ledger.clone(),
        ),
        replacement:
            crate::integration::process_managers::loan_replacement::LoanReplacementWorker::new(
                loans, ledger,
            ),
    }
}

/// Portfolio cash settlement runner coordinated through the Ledger contract.
pub struct PortfolioSettlementRunner {
    cash: crate::contexts::portfolio::application::cash_settlement::PortfolioCashSettlementService<
        crate::contexts::portfolio::infrastructure::PgPortfolioCashSettlementRepository,
        LedgerFacade,
    >,
}
impl PortfolioSettlementRunner {
    pub async fn run_once(&self) -> anyhow::Result<WorkerRunReport> {
        let r = self.cash.run_once().await?;
        Ok(WorkerRunReport {
            claimed: r.claimed,
            records: u32::from(r.posted),
            replayed: 0,
            retry_scheduled: r.retry_due,
            fenced: false,
            dead_lettered: 0,
        })
    }
}
pub fn portfolio_settlement_runner(pool: &VerifiedDatabase) -> PortfolioSettlementRunner {
    let categories = crate::contexts::classification::build(pool);
    let ledger = crate::contexts::ledger::build_with_categories(pool, categories);
    PortfolioSettlementRunner {
        cash: crate::contexts::portfolio::application::cash_settlement::PortfolioCashSettlementService::new(
            crate::contexts::portfolio::infrastructure::PgPortfolioCashSettlementRepository::new(
                pool.pool().clone(),
            ),
            ledger,
        ),
    }
}

/// Sharing bill and settlement workflows coordinated through public contracts.
pub struct SharingWorkflowRunner {
    worker: crate::integration::process_managers::sharing_workflow::SharingWorkflowWorker,
}

impl SharingWorkflowRunner {
    pub async fn run_once(&self) -> anyhow::Result<WorkerRunReport> {
        let report = self.worker.run_once().await?;
        Ok(WorkerRunReport {
            claimed: report.claimed,
            records: u32::from(report.posted),
            replayed: 0,
            retry_scheduled: report.retry_due,
            fenced: false,
            dead_lettered: 0,
        })
    }
}

pub fn sharing_workflow_runner(contexts: &ContextFacades) -> SharingWorkflowRunner {
    SharingWorkflowRunner {
        worker: crate::integration::process_managers::sharing_workflow::SharingWorkflowWorker::new(
            contexts.sharing.clone(),
            contexts.ledger.clone(),
        ),
    }
}

/// Banking accounting and reconciliation retries coordinated only through
/// public Banking and Ledger contracts.
pub struct BankingWorkers {
    banking: BankingFacade,
    ledger: LedgerFacade,
    callback_base: reqwest::Url,
}

impl BankingWorkers {
    pub async fn run_once(&self) -> anyhow::Result<WorkerRunReport> {
        use super::workers::WorkerOperation;

        let now = chrono::Utc::now();
        let mut report = WorkerRunReport::default();
        for step in [
            self.banking
                .run_validation_once("finance-v2-banking-validation", now)
                .await
                .context(WorkerOperation("banking.validation"))?,
            self.banking
                .run_webhook_registration_once(
                    "finance-v2-banking-webhook-registration",
                    self.callback_base.as_str(),
                    now,
                )
                .await
                .context(WorkerOperation("banking.webhook_registration"))?,
            self.banking
                .run_webhook_receipt_once("finance-v2-banking-webhook-receipt", now)
                .await
                .context(WorkerOperation("banking.webhook_receipt"))?,
            self.banking
                .run_statement_once("finance-v2-banking-statement", now)
                .await
                .context(WorkerOperation("banking.statement"))?,
        ] {
            report.claimed |= step.claimed;
            report.records = report.records.saturating_add(step.records);
            report.retry_scheduled |= step.retry_scheduled;
            report.fenced |= step.fenced;
        }
        if let Some((user_id, event_id)) = self
            .banking
            .next_provider_import_candidate()
            .await
            .context(WorkerOperation("banking.import_candidate"))?
        {
            let outcome =
                crate::integration::process_managers::banking_import::import_provider_revision(
                    &self.banking,
                    &self.ledger,
                    user_id,
                    event_id,
                )
                .await
                .context(WorkerOperation("banking.import"))?;
            report.claimed = true;
            report.records = report.records.saturating_add(u32::from(!outcome.replayed));
        }
        if let Some((user_id, observation_id)) = self
            .banking
            .next_balance_observation_candidate()
            .await
            .context(WorkerOperation("banking.balance_candidate"))?
        {
            let outcome = crate::integration::process_managers::banking_observation::deliver_balance_observation(
                &self.banking,
                &self.ledger,
                user_id,
                observation_id,
            )
            .await.context(WorkerOperation("banking.balance_delivery"))?;
            report.claimed = true;
            report.records = report.records.saturating_add(u32::from(!outcome.replayed));
        }
        let finalized = self
            .banking
            .finalize_sync_page_once(now)
            .await
            .context(WorkerOperation("banking.finalize_sync_page"))?;
        report.claimed |= finalized.claimed;
        report.records = report.records.saturating_add(finalized.records);
        report.fenced |= finalized.fenced;
        Ok(report)
    }
}

pub fn banking_workers(contexts: &ContextFacades) -> BankingWorkers {
    banking_workers_with_webhook(
        contexts,
        reqwest::Url::parse("https://localhost/").expect("static worker callback URL is valid"),
    )
}

pub fn banking_workers_with_webhook(
    contexts: &ContextFacades,
    callback_base: reqwest::Url,
) -> BankingWorkers {
    BankingWorkers {
        banking: contexts.banking.clone(),
        ledger: contexts.ledger.clone(),
        callback_base,
    }
}

/// Builds and runs the complete Moneykeeper HTTP and worker composition from a
/// verified database. No unchecked PostgreSQL pool can enter this boundary.
pub async fn run<F>(
    listener: tokio::net::TcpListener,
    pool: &VerifiedDatabase,
    jwks: Arc<JwkSet>,
    secrets: &RuntimeSecrets,
    monobank_webhook_base_url: &reqwest::Url,
    shutdown: F,
) -> anyhow::Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let contexts = build_contexts_with_secrets(pool, secrets);
    let workers =
        crate::bootstrap::workers::production(pool, &contexts, secrets, monobank_webhook_base_url)?;
    let business_router = crate::api::routes::router(contexts, jwks);
    serve(
        listener,
        business_router,
        workers,
        Readiness::default(),
        shutdown,
    )
    .await
}

/// Serves a prepared Moneykeeper router behind the worker/readiness barrier.
///
/// The supplied listener begins with readiness false. Worker startup failure
/// shuts it down without ever allowing business traffic. During shutdown,
/// readiness is removed before HTTP is drained and workers are stopped.
pub async fn serve<F>(
    listener: tokio::net::TcpListener,
    business_router: Router,
    workers: WorkerRegistry,
    readiness: Readiness,
    shutdown: F,
) -> anyhow::Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let serving_started = Instant::now();
    tracing::info!(
        event.name = "app.lifecycle",
        stage = "http",
        outcome = "starting",
        duration_ms = 0_u64,
        "Application lifecycle transition"
    );
    let health = Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .with_state(readiness.clone());
    let business = business_router.layer(middleware::from_fn_with_state(
        readiness.clone(),
        require_readiness,
    ));
    let application = health
        .merge(business)
        .layer(middleware::from_fn(crate::api::middleware::trace_request));

    let (stop_http, mut stop_http_rx) = tokio::sync::watch::channel(false);
    let http = tokio::spawn(async move {
        axum::serve(listener, application)
            .with_graceful_shutdown(async move {
                while !*stop_http_rx.borrow() {
                    if stop_http_rx.changed().await.is_err() {
                        break;
                    }
                }
            })
            .await
    });
    tokio::task::yield_now().await;

    let worker_barrier_started = Instant::now();
    let worker_runtime = match workers.start().await {
        Ok(runtime) => runtime,
        Err(error) => {
            readiness.mark_not_ready();
            tracing::error!(
                event.name = "app.lifecycle",
                stage = "worker_barrier",
                outcome = "failed",
                error.category = "worker.startup",
                error.message = "worker startup barrier failed",
                duration_ms = elapsed_ms(worker_barrier_started.elapsed()),
                "Application lifecycle transition"
            );
            let _ = stop_http.send(true);
            http.await
                .context("join not-ready Moneykeeper HTTP listener")??;
            return Err(error.context("Moneykeeper worker barrier failed"));
        }
    };
    readiness.mark_ready();
    tracing::info!(
        event.name = "app.lifecycle",
        stage = "readiness",
        outcome = "ready",
        duration_ms = elapsed_ms(serving_started.elapsed()),
        "Application lifecycle transition"
    );

    shutdown.await;
    let shutdown_started = Instant::now();
    readiness.mark_not_ready();
    tracing::info!(
        event.name = "app.lifecycle",
        stage = "readiness",
        outcome = "not_ready",
        duration_ms = 0_u64,
        "Application lifecycle transition"
    );
    tracing::info!(
        event.name = "app.lifecycle",
        stage = "http_draining",
        outcome = "started",
        duration_ms = 0_u64,
        "Application lifecycle transition"
    );
    let _ = stop_http.send(true);
    http.await.context("join Moneykeeper HTTP listener")??;
    tracing::info!(
        event.name = "app.lifecycle",
        stage = "http_draining",
        outcome = "completed",
        duration_ms = elapsed_ms(shutdown_started.elapsed()),
        "Application lifecycle transition"
    );
    let worker_shutdown_started = Instant::now();
    tracing::info!(
        event.name = "app.lifecycle",
        stage = "worker_shutdown",
        outcome = "started",
        duration_ms = 0_u64,
        "Application lifecycle transition"
    );
    worker_runtime.shutdown().await?;
    tracing::info!(
        event.name = "app.lifecycle",
        stage = "worker_shutdown",
        outcome = "completed",
        duration_ms = elapsed_ms(worker_shutdown_started.elapsed()),
        "Application lifecycle transition"
    );
    Ok(())
}

pub fn elapsed_ms(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

async fn live() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({"status": "live"})))
}

async fn ready(State(readiness): State<Readiness>) -> Response {
    if readiness.is_ready() {
        (StatusCode::OK, Json(json!({"status": "ready"}))).into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"status": "not_ready"})),
        )
            .into_response()
    }
}

async fn require_readiness(
    State(readiness): State<Readiness>,
    request: Request,
    next: Next,
) -> Response {
    if readiness.is_ready() {
        next.run(request).await
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "not_ready"})),
        )
            .into_response()
    }
}
