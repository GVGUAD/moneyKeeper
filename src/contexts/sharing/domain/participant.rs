//! Participant identity and opaque Ledger references.

use super::ContactId;
use crate::define_uuid_id;

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(tag = "kind", content = "contact_id", rename_all = "snake_case")]
pub enum Participant {
    CurrentUser,
    Contact(ContactId),
}

define_uuid_id!(
    /// Opaque reference to a Ledger journal.
    pub LedgerJournalReference
);
define_uuid_id!(
    /// Opaque reference to a Ledger account.
    pub LedgerAccountReference
);
