//! Periodic Moneykeeper worker registry and graceful cancellation barrier.

use std::collections::HashSet;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, ensure};
use async_trait::async_trait;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::Instrument as _;

use super::runtime::WorkerRunReport;

type WorkerFuture = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'static>>;
type WorkerAction = Arc<dyn Fn() -> WorkerFuture + Send + Sync>;

/// The top-level capabilities that must have exactly one periodic runner.
pub const REQUIRED_WORKERS: &[&str] = &[
    "banking-sync",
    "mail-sync",
    "recurring-lifecycle",
    "recurring-event-policy",
    "outbox-dispatch",
    "process-manager-retries",
    "reporting-projections",
    "reference-data-sync",
    "classification-intake",
    "classification-model",
    "classification-application",
    "classification-backfill",
];

/// Application readiness shared by the HTTP gate and startup/shutdown logic.
#[derive(Clone, Default)]
pub struct Readiness {
    ready: Arc<AtomicBool>,
    ready_transitions: Arc<AtomicUsize>,
}

impl Readiness {
    /// Returns whether business traffic may be served.
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    /// Returns how many times this process transitioned from not-ready to ready.
    pub fn ready_transitions(&self) -> usize {
        self.ready_transitions.load(Ordering::Acquire)
    }

    pub(crate) fn mark_ready(&self) {
        if self
            .ready
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.ready_transitions.fetch_add(1, Ordering::AcqRel);
        }
    }

    pub(crate) fn mark_not_ready(&self) {
        self.ready.store(false, Ordering::Release);
    }
}

/// One uniquely named periodic worker and its startup check.
pub struct WorkerDefinition {
    name: String,
    interval: Duration,
    startup: WorkerAction,
    run_once: WorkerAction,
}

impl fmt::Debug for WorkerDefinition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkerDefinition")
            .field("name", &self.name)
            .field("interval", &self.interval)
            .finish_non_exhaustive()
    }
}

impl WorkerDefinition {
    /// Defines a periodic worker. The first invocation happens immediately.
    pub fn new<F, Fut>(name: impl Into<String>, interval: Duration, run_once: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        Self {
            name: name.into(),
            interval,
            startup: Arc::new(|| Box::pin(async { Ok(()) })),
            run_once: Arc::new(move || Box::pin(run_once())),
        }
    }

    fn new_reported<F, Fut>(name: &'static str, interval: Duration, run_once: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<WorkerRunReport>> + Send + 'static,
    {
        Self::new(name, interval, move || {
            let future = run_once();
            async move {
                let started = Instant::now();
                let report = future.await?;
                log_report(name, report, started.elapsed());
                Ok(())
            }
        })
    }

    /// Adds a startup check that must pass before any worker is spawned.
    pub fn with_startup<F, Fut>(mut self, startup: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        self.startup = Arc::new(move || Box::pin(startup()));
        self
    }
}

/// Validated collection of all worker definitions for one process replica.
pub struct WorkerRegistry {
    definitions: Vec<WorkerDefinition>,
}

impl fmt::Debug for WorkerRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkerRegistry")
            .field("definitions", &self.definitions)
            .finish()
    }
}

impl WorkerRegistry {
    /// Validates unique, printable names and positive intervals.
    pub fn new(definitions: Vec<WorkerDefinition>) -> anyhow::Result<Self> {
        let mut names = HashSet::new();
        for definition in &definitions {
            ensure!(
                !definition.name.is_empty()
                    && definition.name.len() <= 100
                    && definition.name.trim() == definition.name
                    && !definition.name.chars().any(char::is_control),
                "invalid worker name"
            );
            ensure!(
                !definition.interval.is_zero(),
                "worker {} has a zero interval",
                definition.name
            );
            ensure!(
                names.insert(definition.name.clone()),
                "duplicate worker {}",
                definition.name
            );
        }
        Ok(Self { definitions })
    }

    /// Ensures every required capability appears exactly once.
    pub fn require(mut self, required: &[&str]) -> anyhow::Result<Self> {
        let names: HashSet<&str> = self
            .definitions
            .iter()
            .map(|definition| definition.name.as_str())
            .collect();
        for name in required {
            ensure!(names.contains(name), "required worker {name} is missing");
        }
        ensure!(
            names.len() == required.len(),
            "worker registry contains an unexpected capability"
        );
        self.definitions
            .sort_by(|left, right| left.name.cmp(&right.name));
        Ok(self)
    }

    /// Runs every startup check, then spawns each validated worker once.
    pub async fn start(self) -> anyhow::Result<WorkerRuntime> {
        let started = Instant::now();
        let worker_count = self.definitions.len();
        for definition in &self.definitions {
            (definition.startup)()
                .await
                .with_context(|| format!("initialize {} worker", definition.name))?;
        }

        let (shutdown, receiver) = watch::channel(false);
        let mut handles = Vec::with_capacity(self.definitions.len());
        for definition in self.definitions {
            handles.push(spawn_worker(definition, receiver.clone()));
        }
        tracing::info!(
            event.name = "app.lifecycle",
            stage = "worker_barrier",
            outcome = "ready",
            worker_count,
            duration_ms = elapsed_ms(started.elapsed()),
            "Application lifecycle transition"
        );
        Ok(WorkerRuntime { shutdown, handles })
    }
}

fn spawn_worker(
    definition: WorkerDefinition,
    mut shutdown: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let worker_started = Instant::now();
        tracing::info!(
            event.name = "app.lifecycle",
            stage = "worker",
            outcome = "started",
            worker = %definition.name,
            duration_ms = 0_u64,
            "Application lifecycle transition"
        );
        let mut ticker = tokio::time::interval(definition.interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
                _ = ticker.tick() => {
                    let started = Instant::now();
                    let span = tracing::info_span!(
                        "worker.iteration",
                        worker = %definition.name,
                    );
                    if let Err(_error) = (definition.run_once)().instrument(span).await {
                        tracing::warn!(
                            event.name = "worker.iteration.completed",
                            worker = %definition.name,
                            outcome = "error",
                            duration_ms = elapsed_ms(started.elapsed()),
                            error.category = "worker.iteration",
                            error.message = "worker iteration failed",
                            "Worker iteration completed"
                        );
                    }
                }
            }
        }
        tracing::info!(
            event.name = "app.lifecycle",
            stage = "worker",
            outcome = "stopped",
            worker = %definition.name,
            duration_ms = elapsed_ms(worker_started.elapsed()),
            "Application lifecycle transition"
        );
    })
}

fn log_report(worker: &'static str, report: WorkerRunReport, elapsed: Duration) {
    let duration_ms = elapsed_ms(elapsed);
    if !report.claimed {
        tracing::trace!(
            event.name = "worker.iteration.completed",
            worker,
            outcome = "idle",
            duration_ms,
            "Worker iteration completed"
        );
    } else if report.dead_lettered > 0 {
        tracing::warn!(
            event.name = "worker.iteration.completed",
            worker,
            outcome = "dead_lettered",
            records = report.records,
            replayed = report.replayed,
            retry_scheduled = report.retry_scheduled,
            fenced = report.fenced,
            dead_lettered = report.dead_lettered,
            duration_ms,
            "Worker iteration completed"
        );
    } else if report.retry_scheduled || report.fenced {
        tracing::warn!(
            event.name = "worker.iteration.completed",
            worker,
            outcome = if report.fenced {
                "fenced"
            } else {
                "retry_scheduled"
            },
            records = report.records,
            replayed = report.replayed,
            retry_scheduled = report.retry_scheduled,
            fenced = report.fenced,
            duration_ms,
            "Worker iteration completed"
        );
    } else {
        tracing::info!(
            event.name = "worker.iteration.completed",
            worker,
            outcome = "success",
            records = report.records,
            replayed = report.replayed,
            duration_ms,
            "Worker iteration completed"
        );
    }
}

pub fn elapsed_ms(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

/// Running worker handles owned by the application lifecycle.
pub struct WorkerRuntime {
    shutdown: watch::Sender<bool>,
    handles: Vec<JoinHandle<()>>,
}

impl WorkerRuntime {
    /// Stops scheduling new claims and waits for in-flight iterations to finish.
    pub async fn shutdown(self) -> anyhow::Result<()> {
        let _ = self.shutdown.send(true);
        for handle in self.handles {
            handle.await.context("join Moneykeeper worker")?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
struct InProcessPublisher;

#[async_trait]
impl crate::integration::outbox::EventPublisher for InProcessPublisher {
    type Error = std::convert::Infallible;

    async fn publish(
        &self,
        _event: &crate::integration::IntegrationEvent,
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Builds the complete Moneykeeper registry from a verified database only.
pub fn production(
    pool: &crate::infrastructure::database::VerifiedDatabase,
    contexts: &super::runtime::ContextFacades,
    secrets: &super::runtime::RuntimeSecrets,
    monobank_webhook_base_url: &reqwest::Url,
) -> anyhow::Result<WorkerRegistry> {
    use crate::integration::outbox::{DispatcherConfig, OutboxDispatcher};

    let banking = Arc::new(super::runtime::banking_workers_with_webhook(
        contexts,
        monobank_webhook_base_url.clone(),
    ));
    let maintenance = Arc::new(super::runtime::context_maintenance_workers(pool, secrets));
    let event_consumers = Arc::new(super::runtime::event_consumers(pool));
    let classification = Arc::new(super::runtime::classification_runtime(
        pool, contexts, secrets,
    )?);
    let loan_accounting = Arc::new(super::runtime::loan_accounting_workers(pool));
    let portfolio_settlement = Arc::new(super::runtime::portfolio_settlement_runner(pool));
    let sharing_workflows = Arc::new(super::runtime::sharing_workflow_runner(contexts));
    let outbox = Arc::new(OutboxDispatcher::new(
        pool,
        "finance-v2-outbox",
        InProcessPublisher,
        DispatcherConfig::default(),
    )?);
    let interval = Duration::from_secs(1);

    WorkerRegistry::new(vec![
        WorkerDefinition::new_reported("banking-sync", interval, move || {
            let banking = Arc::clone(&banking);
            async move { banking.run_once().await }
        }),
        WorkerDefinition::new_reported("mail-sync", interval, {
            let maintenance = Arc::clone(&maintenance);
            move || {
                let maintenance = Arc::clone(&maintenance);
                async move { maintenance.run_mail_once().await }
            }
        }),
        WorkerDefinition::new_reported("recurring-lifecycle", interval, {
            let maintenance = Arc::clone(&maintenance);
            move || {
                let maintenance = Arc::clone(&maintenance);
                async move { maintenance.run_recurring_once().await }
            }
        }),
        WorkerDefinition::new_reported("recurring-event-policy", interval, {
            let event_consumers = Arc::clone(&event_consumers);
            move || {
                let event_consumers = Arc::clone(&event_consumers);
                async move { event_consumers.run_recurring_once().await }
            }
        }),
        WorkerDefinition::new_reported("outbox-dispatch", interval, move || {
            let outbox = Arc::clone(&outbox);
            async move {
                let report = outbox.dispatch_batch().await?;
                Ok(WorkerRunReport {
                    claimed: report.claimed > 0,
                    records: report.published,
                    retry_scheduled: report.retry_scheduled > 0,
                    fenced: report.fenced > 0,
                    dead_lettered: report.dead_lettered,
                    ..WorkerRunReport::default()
                })
            }
        }),
        WorkerDefinition::new_reported("process-manager-retries", interval, move || {
            let loan_accounting = Arc::clone(&loan_accounting);
            let portfolio_settlement = Arc::clone(&portfolio_settlement);
            let sharing_workflows = Arc::clone(&sharing_workflows);
            async move {
                let mut report = WorkerRunReport::default();
                report.merge(loan_accounting.run_opening_once().await?);
                report.merge(loan_accounting.run_accounting_once().await?);
                report.merge(loan_accounting.run_reversal_once().await?);
                report.merge(loan_accounting.run_replacement_once().await?);
                report.merge(portfolio_settlement.run_once().await?);
                report.merge(sharing_workflows.run_once().await?);
                Ok(report)
            }
        }),
        WorkerDefinition::new_reported("reporting-projections", interval, {
            let event_consumers = Arc::clone(&event_consumers);
            move || {
                let event_consumers = Arc::clone(&event_consumers);
                async move { event_consumers.run_reporting_once().await }
            }
        }),
        WorkerDefinition::new_reported("classification-intake", interval, {
            let classification = Arc::clone(&classification);
            move || {
                let classification = Arc::clone(&classification);
                async move {
                    let report = classification.run_intake_once().await?;
                    Ok(WorkerRunReport {
                        claimed: report.claimed,
                        records: report.records,
                        retry_scheduled: report.retry_scheduled,
                        fenced: report.fenced,
                        dead_lettered: u32::from(report.failed),
                        ..WorkerRunReport::default()
                    })
                }
            }
        }),
        WorkerDefinition::new_reported("classification-model", interval, {
            let classification = Arc::clone(&classification);
            move || {
                let classification = Arc::clone(&classification);
                async move {
                    let report = classification.run_classifier_once().await?;
                    Ok(WorkerRunReport {
                        claimed: report.claimed,
                        records: report.records,
                        retry_scheduled: report.retry_scheduled,
                        fenced: report.fenced,
                        dead_lettered: u32::from(report.failed),
                        ..WorkerRunReport::default()
                    })
                }
            }
        }),
        WorkerDefinition::new_reported("classification-application", interval, {
            let classification = Arc::clone(&classification);
            move || {
                let classification = Arc::clone(&classification);
                async move {
                    let report = classification.run_application_once().await?;
                    Ok(WorkerRunReport {
                        claimed: report.claimed,
                        records: report.records,
                        retry_scheduled: report.retry_scheduled,
                        fenced: report.fenced,
                        dead_lettered: u32::from(report.failed),
                        ..WorkerRunReport::default()
                    })
                }
            }
        }),
        WorkerDefinition::new_reported("classification-backfill", interval, {
            let classification = Arc::clone(&classification);
            move || {
                let classification = Arc::clone(&classification);
                async move {
                    let report = classification.run_backfill_once().await?;
                    Ok(WorkerRunReport {
                        claimed: report.claimed,
                        records: report.records,
                        retry_scheduled: report.retry_scheduled,
                        fenced: report.fenced,
                        dead_lettered: u32::from(report.failed),
                        ..WorkerRunReport::default()
                    })
                }
            }
        }),
        WorkerDefinition::new_reported("reference-data-sync", interval, move || {
            let maintenance = Arc::clone(&maintenance);
            async move { maintenance.run_reference_data_once().await }
        }),
    ])?
    .require(REQUIRED_WORKERS)
}

#[cfg(test)]
mod logging_tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::fmt::MakeWriter;

    use super::{WorkerRunReport, log_report};

    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    struct Writer(Arc<Mutex<Vec<u8>>>);

    impl Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> MakeWriter<'writer> for Buffer {
        type Writer = Writer;

        fn make_writer(&'writer self) -> Self::Writer {
            Writer(Arc::clone(&self.0))
        }
    }

    fn capture(report: WorkerRunReport) -> String {
        let output = Buffer::default();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .flatten_event(true)
            .with_ansi(false)
            .with_env_filter(EnvFilter::new("info"))
            .with_writer(output.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            log_report("test-worker", report, Duration::from_millis(9));
        });
        String::from_utf8(output.0.lock().unwrap().clone()).unwrap()
    }

    use std::time::Duration;

    #[test]
    fn idle_iterations_are_suppressed_at_info() {
        assert!(capture(WorkerRunReport::default()).is_empty());
    }

    #[test]
    fn claimed_retry_fence_and_dead_letter_outcomes_are_structured() {
        let success = capture(WorkerRunReport {
            claimed: true,
            records: 3,
            ..WorkerRunReport::default()
        });
        assert!(success.contains("worker.iteration.completed"));
        assert!(success.contains("success"));

        let retry = capture(WorkerRunReport {
            claimed: true,
            retry_scheduled: true,
            ..WorkerRunReport::default()
        });
        assert!(retry.contains("retry_scheduled"));

        let fenced = capture(WorkerRunReport {
            claimed: true,
            fenced: true,
            ..WorkerRunReport::default()
        });
        assert!(fenced.contains("fenced"));

        let dead = capture(WorkerRunReport {
            claimed: true,
            dead_lettered: 1,
            ..WorkerRunReport::default()
        });
        assert!(dead.contains("dead_lettered"));
    }
}
