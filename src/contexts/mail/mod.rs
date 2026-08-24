//! Mail owns encrypted connections and immutable receipt evidence.
#![allow(dead_code, unused_imports)]

pub(crate) mod api;
pub(crate) mod application;
pub mod domain;
pub(crate) mod infrastructure;
pub mod public;

use crate::infrastructure::database::VerifiedDatabase;
pub(crate) fn build(pool: &VerifiedDatabase) -> public::MailFacade {
    let mut key = [0_u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut key);
    build_with_key(pool, "ephemeral-moneykeeper-mail", key)
}

pub(crate) fn build_with_key(
    pool: &VerifiedDatabase,
    key_id: &str,
    key: [u8; 32],
) -> public::MailFacade {
    public::MailFacade::new(
        std::sync::Arc::new(infrastructure::PgMailStore::new(
            pool.pool().clone(),
            infrastructure::MailCrypto::new(key_id, key)
                .expect("validated Moneykeeper Mail key configuration"),
        )),
        std::sync::Arc::new(infrastructure::oauth::GoogleOAuthClient::from_environment()),
    )
}
