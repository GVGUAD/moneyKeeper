// Domain layer: entities, value objects, aggregates, repository traits, domain events.
// No dependencies on application or infrastructure layers.

pub mod account;
pub mod bank_connection;
pub mod category;
pub mod email;
pub mod email_connection;
pub mod error;
pub mod fx_rate;
pub mod monobank;
pub mod receipt_parser;
pub mod stats;
pub mod subscription;
pub mod subscription_charge;
pub mod subscription_error;
pub mod transaction;
pub mod user_settings;
