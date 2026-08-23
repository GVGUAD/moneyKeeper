//! Periodic Finance V2 worker registry and graceful cancellation barrier.

use std::collections::HashSet;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context, ensure};
use async_trait::async_trait;
use tokio::sync::watch;
use tokio::task::JoinHandle;

type WorkerFuture = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'static>>;
type WorkerAction = Arc<dyn Fn() -> WorkerFuture + Send + Sync>;

/// The top-level capabilities that must have exactly one periodic runner.
pub const REQUIRED_WORKERS: &[&str] = &[
    "banking-sync",
    "mail-sync",
    "recurring-lifecycle",
    "outbox-dispatch",
    "process-manager-retries",
    "reporting-consumers",
    "reference-data-sync",
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
        Ok(WorkerRuntime { shutdown, handles })
    }
}

fn spawn_worker(
    definition: WorkerDefinition,
    mut shutdown: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
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
                    if let Err(error) = (definition.run_once)().await {
                        tracing::warn!(worker = %definition.name, ?error, "Finance V2 worker iteration failed");
                    }
                }
            }
        }
    })
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
            handle.await.context("join Finance V2 worker")?;
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

/// Builds the complete Finance V2 registry from a verified database only.
pub fn production(
    pool: &crate::infrastructure::v2_db::VerifiedV2Pool,
    contexts: &super::v2::SupportingContexts,
    secrets: &super::v2::V2Secrets,
) -> anyhow::Result<WorkerRegistry> {
    use crate::integration::outbox::{DispatcherConfig, OutboxDispatcher};

    let banking = Arc::new(super::v2::banking_workers(contexts));
    let phase4 = Arc::new(super::v2::phase4_workers_with_secrets(pool, secrets));
    let phase6 = Arc::new(super::v2::phase6_workers(pool));
    let phase7 = Arc::new(super::v2::phase7_workers(pool));
    let outbox = Arc::new(OutboxDispatcher::new(
        pool,
        "finance-v2-outbox",
        InProcessPublisher,
        DispatcherConfig::default(),
    )?);
    let interval = Duration::from_secs(1);

    WorkerRegistry::new(vec![
        WorkerDefinition::new("banking-sync", interval, move || {
            let banking = Arc::clone(&banking);
            async move {
                banking.run_once().await?;
                Ok(())
            }
        }),
        WorkerDefinition::new("mail-sync", interval, {
            let phase4 = Arc::clone(&phase4);
            move || {
                let phase4 = Arc::clone(&phase4);
                async move {
                    phase4.run_mail_once().await?;
                    Ok(())
                }
            }
        }),
        WorkerDefinition::new("recurring-lifecycle", interval, {
            let phase4 = Arc::clone(&phase4);
            move || {
                let phase4 = Arc::clone(&phase4);
                async move {
                    phase4.run_recurring_once().await?;
                    Ok(())
                }
            }
        }),
        WorkerDefinition::new("outbox-dispatch", interval, move || {
            let outbox = Arc::clone(&outbox);
            async move {
                outbox.dispatch_batch().await?;
                Ok(())
            }
        }),
        WorkerDefinition::new("process-manager-retries", interval, move || {
            let phase6 = Arc::clone(&phase6);
            let phase7 = Arc::clone(&phase7);
            async move {
                phase6.run_opening_once().await?;
                phase6.run_accounting_once().await?;
                phase6.run_reversal_once().await?;
                phase6.run_replacement_once().await?;
                phase7.run_cash_once().await?;
                Ok(())
            }
        }),
        WorkerDefinition::new("reporting-consumers", interval, {
            let phase4 = Arc::clone(&phase4);
            move || {
                let phase4 = Arc::clone(&phase4);
                async move {
                    phase4.route_event_once().await?;
                    Ok(())
                }
            }
        }),
        WorkerDefinition::new("reference-data-sync", interval, move || {
            let phase4 = Arc::clone(&phase4);
            async move {
                phase4.run_nbu_once().await?;
                Ok(())
            }
        }),
    ])?
    .require(REQUIRED_WORKERS)
}
