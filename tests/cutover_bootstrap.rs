use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::{Router, routing::get};
use moneykeeper::bootstrap::serve;
use moneykeeper::bootstrap::workers::{
    REQUIRED_WORKERS, Readiness, WorkerDefinition, WorkerRegistry,
};
use tokio::sync::{Notify, oneshot};

#[tokio::test]
async fn readiness_stays_false_until_worker_barrier_then_flips_once() {
    let readiness = Readiness::default();
    let startup_gate = Arc::new(Notify::new());
    let runs = Arc::new(AtomicUsize::new(0));
    let worker = WorkerDefinition::new("mail-sync", Duration::from_secs(60), {
        let runs = Arc::clone(&runs);
        move || {
            let runs = Arc::clone(&runs);
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }
    })
    .with_startup({
        let startup_gate = Arc::clone(&startup_gate);
        move || {
            let startup_gate = Arc::clone(&startup_gate);
            async move {
                startup_gate.notified().await;
                Ok(())
            }
        }
    });
    let registry = WorkerRegistry::new(vec![worker]).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let server_readiness = readiness.clone();
    let server = tokio::spawn(async move {
        serve(
            listener,
            Router::new().route("/business", get(|| async { "ok" })),
            registry,
            server_readiness,
            async {
                let _ = shutdown_rx.await;
            },
        )
        .await
    });

    let client = reqwest::Client::new();
    let unavailable = client
        .get(format!("http://{address}/business"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        unavailable.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(!readiness.is_ready());

    startup_gate.notify_one();
    for _ in 0..100 {
        if readiness.is_ready() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(readiness.is_ready());
    assert_eq!(readiness.ready_transitions(), 1);
    let available = client
        .get(format!("http://{address}/business"))
        .send()
        .await
        .unwrap();
    assert_eq!(available.status(), reqwest::StatusCode::OK);

    shutdown_tx.send(()).unwrap();
    server.await.unwrap().unwrap();
    assert!(!readiness.is_ready());
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn worker_initialization_failure_never_reaches_readiness() {
    let readiness = Readiness::default();
    let worker = WorkerDefinition::new("broken", Duration::from_secs(60), || async { Ok(()) })
        .with_startup(|| async { anyhow::bail!("invalid secret configuration") });
    let registry = WorkerRegistry::new(vec![worker]).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();

    let error = serve(
        listener,
        Router::new(),
        registry,
        readiness.clone(),
        std::future::pending(),
    )
    .await
    .expect_err("worker startup must fail the application barrier");
    assert!(format!("{error:#}").contains("invalid secret configuration"));
    assert!(!readiness.is_ready());
    assert_eq!(readiness.ready_transitions(), 0);
}

#[test]
fn registry_rejects_duplicate_worker_names() {
    let definition = || {
        WorkerDefinition::new("outbox-dispatch", Duration::from_secs(1), || async {
            Ok(())
        })
    };
    let error = WorkerRegistry::new(vec![definition(), definition()])
        .expect_err("duplicate workers must not start twice");
    assert!(format!("{error:#}").contains("duplicate worker"));
}

#[tokio::test]
async fn worker_registry_starts_every_required_capability_exactly_once() {
    let counts = Arc::new(
        REQUIRED_WORKERS
            .iter()
            .map(|name| ((*name).to_owned(), AtomicUsize::new(0)))
            .collect::<std::collections::HashMap<_, _>>(),
    );
    let definitions = REQUIRED_WORKERS
        .iter()
        .map(|name| {
            let name = (*name).to_owned();
            let counts = Arc::clone(&counts);
            WorkerDefinition::new(name.clone(), Duration::from_secs(60), move || {
                let counts = Arc::clone(&counts);
                let name = name.clone();
                async move {
                    counts[&name].fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            })
        })
        .collect();
    let registry = WorkerRegistry::new(definitions)
        .unwrap()
        .require(REQUIRED_WORKERS)
        .unwrap();
    let runtime = registry.start().await.unwrap();
    for _ in 0..100 {
        if counts
            .values()
            .all(|count| count.load(Ordering::SeqCst) == 1)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    runtime.shutdown().await.unwrap();
    for name in REQUIRED_WORKERS {
        assert_eq!(counts[*name].load(Ordering::SeqCst), 1, "{name}");
    }
}
