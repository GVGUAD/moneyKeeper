//! PostgreSQL and provider adapters for Mail.
pub(crate) mod gmail;
pub(crate) mod oauth;
pub(crate) mod parsers;
mod repository;
pub(crate) mod sync_worker;
pub(crate) mod unit_of_work;
pub(crate) use repository::{
    CallbackResult, MailStoreError, OauthCallbackPreparation, OauthStartResult, PgMailStore,
};
