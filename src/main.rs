use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use moneykeeper::bootstrap::{self, RuntimeConfig};
use moneykeeper::infrastructure::database::initialize_database;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let application_started = Instant::now();
    let logging_started = Instant::now();
    let log_format = moneykeeper::observability::initialize()?;
    tracing::info!(
        event.name = "app.lifecycle",
        stage = "logging",
        outcome = "ready",
        format = ?log_format,
        duration_ms = elapsed_ms(logging_started.elapsed()),
        "Application lifecycle transition"
    );

    match run_application().await {
        Ok(()) => Ok(()),
        Err(error) => {
            tracing::error!(
                event.name = "app.lifecycle",
                stage = "application",
                outcome = "failed",
                error.category = "application.failure",
                error.message = "application terminated with an error",
                duration_ms = elapsed_ms(application_started.elapsed()),
                "Application lifecycle transition"
            );
            Err(error)
        }
    }
}

async fn run_application() -> anyhow::Result<()> {
    // Validate every startup-critical secret and address before touching the
    // database or any external provider.
    let started = Instant::now();
    let config = RuntimeConfig::from_environment()?;
    lifecycle_ready("configuration", started.elapsed());

    let started = Instant::now();
    let pool = initialize_database(config.database_url()).await?;
    lifecycle_ready("database", started.elapsed());

    // The stable lineage marker and complete embedded baseline have now passed.
    // Only after that safety boundary may startup perform external I/O.
    let started = Instant::now();
    let jwks = fetch_jwks(config.jwks_url()).await?;
    lifecycle_ready("jwks", started.elapsed());
    let started = Instant::now();
    let listener = tokio::net::TcpListener::bind(config.bind_address())
        .await
        .context("bind Moneykeeper HTTP listener")?;
    tracing::info!(
        event.name = "app.lifecycle",
        stage = "listener",
        outcome = "bound",
        address = %config.bind_address(),
        duration_ms = elapsed_ms(started.elapsed()),
        "Application lifecycle transition"
    );

    bootstrap::run(
        listener,
        &pool,
        jwks,
        config.secrets(),
        config.monobank_webhook_base_url(),
        shutdown_signal(),
    )
    .await
}

async fn fetch_jwks(url: reqwest::Url) -> anyhow::Result<Arc<jsonwebtoken::jwk::JwkSet>> {
    let started = Instant::now();
    let response = match reqwest::get(url).await {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(
                event.name = "provider.request.completed",
                provider = "supabase",
                operation = "jwks",
                outcome = transport_outcome(&error),
                duration_ms = elapsed_ms(started.elapsed()),
                "Provider request completed"
            );
            return Err(error).context("fetch Supabase JWKS");
        }
    };
    let status = response.status();
    if !status.is_success() {
        tracing::warn!(
            event.name = "provider.request.completed",
            provider = "supabase",
            operation = "jwks",
            outcome = "http_error",
            http.status = status.as_u16(),
            duration_ms = elapsed_ms(started.elapsed()),
            "Provider request completed"
        );
        return Err(response
            .error_for_status()
            .expect_err("non-success status produces a reqwest error"))
        .context("Supabase JWKS endpoint rejected startup");
    }
    let jwks = match response.json().await {
        Ok(jwks) => jwks,
        Err(error) => {
            tracing::warn!(
                event.name = "provider.request.completed",
                provider = "supabase",
                operation = "jwks",
                outcome = "decode_error",
                http.status = status.as_u16(),
                duration_ms = elapsed_ms(started.elapsed()),
                "Provider request completed"
            );
            return Err(error).context("decode Supabase JWKS");
        }
    };
    tracing::info!(
        event.name = "provider.request.completed",
        provider = "supabase",
        operation = "jwks",
        outcome = "success",
        http.status = status.as_u16(),
        duration_ms = elapsed_ms(started.elapsed()),
        "Provider request completed"
    );
    Ok(Arc::new(jwks))
}

fn lifecycle_ready(stage: &'static str, elapsed: Duration) {
    tracing::info!(
        event.name = "app.lifecycle",
        stage,
        outcome = "ready",
        duration_ms = elapsed_ms(elapsed),
        "Application lifecycle transition"
    );
}

fn elapsed_ms(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

fn transport_outcome(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect_error"
    } else {
        "transport_error"
    }
}

async fn shutdown_signal() {
    let started = Instant::now();
    if let Err(_error) = tokio::signal::ctrl_c().await {
        tracing::error!(
            event.name = "app.lifecycle",
            stage = "shutdown_signal",
            outcome = "failed",
            error.category = "shutdown.signal",
            error.message = "shutdown signal listener failed",
            duration_ms = elapsed_ms(started.elapsed()),
            "Application lifecycle transition"
        );
    } else {
        tracing::info!(
            event.name = "app.lifecycle",
            stage = "shutdown_signal",
            outcome = "received",
            duration_ms = elapsed_ms(started.elapsed()),
            "Application lifecycle transition"
        );
    }
}
