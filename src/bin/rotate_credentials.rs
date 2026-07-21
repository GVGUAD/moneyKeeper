use std::sync::Arc;

use moneykeeper::infrastructure::credential_crypto::{CredentialRotationService, TokenCipher};
use moneykeeper::infrastructure::db::create_pool;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let database_url =
        std::env::var("DATABASE_URL").map_err(|_| anyhow::anyhow!("DATABASE_URL must be set"))?;
    let pool = create_pool(&database_url).await?;
    let cipher = Arc::new(TokenCipher::from_env()?);
    let service = CredentialRotationService::new(pool, cipher);
    let sanitize = std::env::args().any(|argument| argument == "--sanitize-plaintext");
    let report = if sanitize {
        service.sanitize_plaintext().await?
    } else {
        service.run().await?
    };
    println!(
        "credential {} complete: bank_connections={}, email_connections={}",
        if sanitize { "sanitization" } else { "rotation" },
        report.bank_connections,
        report.email_connections
    );
    Ok(())
}
