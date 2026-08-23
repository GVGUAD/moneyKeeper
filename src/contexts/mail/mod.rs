//! Mail owns encrypted connections and immutable receipt evidence.
#![allow(dead_code, unused_imports)]

pub(crate) mod api;
pub(crate) mod application;
pub mod domain;
pub(crate) mod infrastructure;
pub mod public;

use crate::infrastructure::v2_db::VerifiedV2Pool;
pub(crate) fn build(pool: &VerifiedV2Pool) -> public::MailFacade {
    let mut key = [0_u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut key);
    build_with_key(pool, "ephemeral-v2-mail", key)
}

pub(crate) fn build_with_key(
    pool: &VerifiedV2Pool,
    key_id: &str,
    key: [u8; 32],
) -> public::MailFacade {
    public::MailFacade::new(
        infrastructure::PgMailStore::new(
            pool.pool().clone(),
            infrastructure::MailCrypto::new(key_id, key)
                .expect("validated Finance V2 Mail key configuration"),
        ),
        std::sync::Arc::new(infrastructure::oauth::GoogleOAuthClient::from_environment()),
    )
}
