use std::sync::Arc;

use anyhow::Context;
use moneykeeper::bootstrap::{self, RuntimeConfig};
use moneykeeper::infrastructure::database::initialize_database;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    // Validate every startup-critical secret and address before touching the
    // database or any external provider.
    let config = RuntimeConfig::from_environment()?;
    let pool = initialize_database(config.database_url()).await?;

    // The stable lineage marker and complete embedded baseline have now passed.
    // Only after that safety boundary may startup perform external I/O.
    let jwks: jsonwebtoken::jwk::JwkSet = reqwest::get(config.jwks_url())
        .await
        .context("fetch Supabase JWKS")?
        .error_for_status()
        .context("Supabase JWKS endpoint rejected startup")?
        .json()
        .await
        .context("decode Supabase JWKS")?;
    let listener = tokio::net::TcpListener::bind(config.bind_address())
        .await
        .context("bind Moneykeeper HTTP listener")?;
    tracing::info!(address = %config.bind_address(), "Moneykeeper listener bound with readiness false");

    bootstrap::run(
        listener,
        &pool,
        Arc::new(jwks),
        config.secrets(),
        config.monobank_webhook_base_url(),
        shutdown_signal(),
    )
    .await
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(?error, "failed to install shutdown signal");
    }
}
