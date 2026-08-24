//! Cross-context process-manager compositions.
//!
//! Workflows here depend only on context public contracts and the durable
//! integration runtime.
pub mod banking_import;
pub mod banking_observation;
pub mod banking_resource_mapping;
pub mod loan_accounting;
pub mod loan_opening;
pub mod loan_replacement;
pub mod loan_reversal;
pub mod recurring_match;
pub mod sharing_accounting;
pub mod sharing_settlement;
pub mod sharing_workflow;
