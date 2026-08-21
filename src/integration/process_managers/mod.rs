//! Cross-context process-manager compositions.
//!
//! Phase 1 deliberately leaves this module empty. Later phases add workflows
//! that depend only on context public contracts and the durable runtime.
pub mod banking_import;
pub mod banking_observation;
pub mod banking_resource_mapping;
pub mod loan_accounting;
pub mod loan_opening;
pub mod loan_replacement;
pub mod loan_reversal;
pub(crate) mod phase4_router;
pub mod recurring_match;
