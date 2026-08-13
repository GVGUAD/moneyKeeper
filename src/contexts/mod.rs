//! Finance V2 bounded contexts.
//!
//! Each context keeps its implementation private and exposes collaboration
//! contracts through its `public` module only.

pub mod banking;
pub mod classification;
pub mod ledger;
pub mod loans;
pub mod mail;
pub mod portfolio;
pub mod preferences;
pub mod recurring;
pub mod reference_data;
pub mod reporting;
pub mod sharing;
