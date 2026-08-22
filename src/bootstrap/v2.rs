//! Isolated Finance V2 composition root.

use std::{sync::Arc, time::Duration};

use axum::Router;
use jsonwebtoken::jwk::JwkSet;

use crate::contexts::banking::public::{Aes256CredentialCipher, BankingFacade, MonobankClient};
use crate::contexts::classification::public::CategoryCatalogFacade;
use crate::contexts::ledger::public::LedgerFacade;
use crate::contexts::loans::public::LoansFacade;
use crate::contexts::mail::public::MailFacade;
use crate::contexts::portfolio::public::PortfolioFacade;
use crate::contexts::preferences::public::PreferencesFacade;
use crate::contexts::recurring::public::RecurringFacade;
use crate::contexts::reference_data::public::CurrencyCatalogFacade;
use crate::contexts::reporting::public::ReportingFacade;
use crate::contexts::sharing::public::SharingFacade;
use crate::infrastructure::v2_db::VerifiedV2Pool;

/// Public supporting-context capabilities assembled only after V2 lineage
/// verification. Concrete PostgreSQL adapters remain context-private.
#[derive(Clone)]
pub struct SupportingContexts {
    pub currencies: CurrencyCatalogFacade,
    pub categories: CategoryCatalogFacade,
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

/// Builds all Phase 1 supporting capabilities from a verified database.
pub fn supporting_contexts(pool: &VerifiedV2Pool) -> SupportingContexts {
    let categories = crate::contexts::classification::build(pool);
    let currencies = crate::contexts::reference_data::build(pool);
    let ledger = crate::contexts::ledger::build_with_categories(pool, categories.clone());
    let banking = crate::contexts::banking::build_with_ledger(
        pool,
        Arc::new(
            Aes256CredentialCipher::new("parallel-v2-banking", [0x42; 32])
                .expect("the static parallel V2 key has the required length"),
        ),
        Arc::new(MonobankClient::new("https://api.monobank.ua")),
        ledger.clone(),
        currencies.clone(),
        [0x24; 32],
    );
    SupportingContexts {
        currencies,
        categories: categories.clone(),
        preferences: crate::contexts::preferences::build(pool),
        ledger,
        banking,
        mail: crate::contexts::mail::build(pool),
        recurring: crate::contexts::recurring::build(pool),
        reporting: crate::contexts::reporting::build(pool),
        loans: crate::contexts::loans::build(pool),
        sharing: crate::contexts::sharing::build(pool),
        portfolio: crate::contexts::portfolio::build(pool),
    }
}

/// Builds the isolated Finance V2 supporting-context router.
///
/// This function does not spawn workers and is intentionally unused by
/// `main.rs` before the Phase 8 cutover.
pub fn router(pool: &VerifiedV2Pool, jwks: Arc<JwkSet>) -> Router {
    crate::api::v2::router(supporting_contexts(pool), jwks)
}

/// Bounded Phase 4 worker entry points. Timers may call these methods, but all
/// durable scheduling, cursor, retry, and fencing state remains in PostgreSQL.
pub struct Phase4Workers {
    mail: crate::contexts::mail::infrastructure::sync_worker::MailSyncWorker<
        crate::contexts::mail::infrastructure::gmail::GmailClient,
        crate::contexts::mail::infrastructure::oauth::GoogleOAuthClient,
    >,
    fx: crate::contexts::reference_data::infrastructure::nbu::NbuSyncWorker<
        crate::contexts::reference_data::infrastructure::nbu::NbuClient,
    >,
    recurring:
        crate::contexts::recurring::infrastructure::categorization_worker::CategorizationWorker,
    events: crate::integration::process_managers::phase4_router::Phase4EventRouter,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkerRunReport {
    pub claimed: bool,
    pub records: u32,
    pub replayed: u32,
    pub retry_scheduled: bool,
    pub fenced: bool,
}

impl Phase4Workers {
    pub async fn run_mail_once(&self) -> anyhow::Result<WorkerRunReport> {
        let report = self.mail.run_once().await?;
        Ok(WorkerRunReport {
            claimed: report.claimed,
            records: report.messages_recorded,
            replayed: 0,
            retry_scheduled: report.retry_scheduled,
            fenced: report.fenced,
        })
    }

    pub async fn run_nbu_once(&self) -> anyhow::Result<WorkerRunReport> {
        let report = self.fx.run_once().await?;
        Ok(WorkerRunReport {
            claimed: report.claimed,
            records: report.observations,
            replayed: report.replayed,
            retry_scheduled: report.retry_scheduled,
            fenced: report.fenced,
        })
    }

    pub async fn run_recurring_once(&self) -> anyhow::Result<WorkerRunReport> {
        let report = self.recurring.run_once().await?;
        Ok(WorkerRunReport {
            claimed: report.claimed,
            records: u32::from(report.posted || report.compensated),
            replayed: 0,
            retry_scheduled: report.retry_scheduled,
            fenced: report.fenced,
        })
    }

    pub async fn route_event_once(&self) -> anyhow::Result<WorkerRunReport> {
        let report = self.events.run_once().await?;
        Ok(WorkerRunReport {
            claimed: report.routed || report.ignored,
            records: u32::from(report.routed),
            replayed: 0,
            retry_scheduled: false,
            fenced: false,
        })
    }
}

/// Constructs workers without spawning them or changing the legacy runtime.
pub fn phase4_workers(pool: &VerifiedV2Pool) -> Phase4Workers {
    let categories = crate::contexts::classification::build(pool);
    let ledger = crate::contexts::ledger::build_with_categories(pool, categories);
    let recurring = crate::contexts::recurring::build(pool);
    let reporting = crate::contexts::reporting::build(pool);
    Phase4Workers {
        mail: crate::contexts::mail::infrastructure::sync_worker::MailSyncWorker::new(
            pool.pool().clone(),
            crate::contexts::mail::infrastructure::gmail::GmailClient::new(
                "https://gmail.googleapis.com",
            ),
            crate::contexts::mail::infrastructure::oauth::GoogleOAuthClient::from_environment(),
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
        events: crate::integration::process_managers::phase4_router::Phase4EventRouter::new(
            pool.pool().clone(),
            ledger,
            recurring,
            reporting,
        ),
    }
}

/// Explicit Phase 6 worker entry points. They are constructed only by the V2
/// composition root and are never started by the legacy runtime.
pub struct Phase6Workers {
    opening: crate::integration::process_managers::loan_opening::LoanOpeningWorker,
    accounting: crate::integration::process_managers::loan_accounting::LoanAccountingWorker,
    reversal: crate::integration::process_managers::loan_reversal::LoanReversalWorker,
    replacement: crate::integration::process_managers::loan_replacement::LoanReplacementWorker,
}

impl Phase6Workers {
    pub async fn run_opening_once(&self) -> anyhow::Result<WorkerRunReport> {
        let report = self.opening.run_once().await?;
        Ok(WorkerRunReport {
            claimed: report.claimed,
            records: u32::from(report.posted),
            replayed: 0,
            retry_scheduled: report.retry_due,
            fenced: false,
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
        })
    }
}

pub fn phase6_workers(pool: &VerifiedV2Pool) -> Phase6Workers {
    let categories = crate::contexts::classification::build(pool);
    let ledger = crate::contexts::ledger::build_with_categories(pool, categories);
    let loans = crate::contexts::loans::build(pool);
    Phase6Workers {
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

/// Explicit Phase 7 Portfolio cash worker; never started by the legacy runtime.
pub struct Phase7Workers {
    cash: crate::contexts::portfolio::infrastructure::cash_worker::PortfolioCashSettlementWorker,
}
impl Phase7Workers {
    pub async fn run_cash_once(&self) -> anyhow::Result<WorkerRunReport> {
        let r = self.cash.run_once().await?;
        Ok(WorkerRunReport {
            claimed: r.claimed,
            records: u32::from(r.posted),
            replayed: 0,
            retry_scheduled: r.retry_due,
            fenced: false,
        })
    }
}
pub fn phase7_workers(pool: &VerifiedV2Pool) -> Phase7Workers {
    let categories = crate::contexts::classification::build(pool);
    let ledger = crate::contexts::ledger::build_with_categories(pool, categories);
    Phase7Workers{cash:crate::contexts::portfolio::infrastructure::cash_worker::PortfolioCashSettlementWorker::new(pool.pool().clone(),ledger)}
}

/// Phase 5 cross-context coordinators, built only for the isolated V2 lineage.
pub struct Phase5Coordinators {
    pub accounting:
        crate::integration::process_managers::sharing_accounting::SharingAccountingCoordinator,
    pub settlement:
        crate::integration::process_managers::sharing_settlement::SharingSettlementCoordinator,
    pub reporting: ReportingFacade,
}

pub fn phase5_coordinators(pool: &VerifiedV2Pool) -> Phase5Coordinators {
    let categories = crate::contexts::classification::build(pool);
    let ledger = crate::contexts::ledger::build_with_categories(pool, categories);
    Phase5Coordinators {
        accounting: crate::integration::process_managers::sharing_accounting::SharingAccountingCoordinator::new(ledger.clone()),
        settlement: crate::integration::process_managers::sharing_settlement::SharingSettlementCoordinator::new(ledger),
        reporting: crate::contexts::reporting::build(pool),
    }
}
