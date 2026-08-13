//! Private SQL row mappings kept outside the domain model.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::shared_kernel::{CurrencyCode, UserId};

use super::super::domain::{
    AccountAuthority, AccountKind, AccountLifecycle, AccountNature, AccountVersion,
    AccountVisibility, LedgerAccount, LedgerAccountId, LedgerError, SystemAccountRole,
};

#[derive(sqlx::FromRow)]
pub(super) struct AccountRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub currency: String,
    pub nature: String,
    pub kind: String,
    pub authority: String,
    pub visibility: String,
    pub lifecycle: String,
    pub system_role: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AccountRow {
    pub(super) fn into_domain(self) -> Result<LedgerAccount, LedgerError> {
        LedgerAccount::rehydrate(
            LedgerAccountId::new(self.id),
            UserId::new(self.user_id),
            self.name,
            CurrencyCode::new(self.currency)
                .map_err(|_| LedgerError::persistence("stored account currency is invalid"))?,
            AccountNature::parse(&self.nature)?,
            AccountKind::parse(&self.kind)?,
            AccountAuthority::parse(&self.authority)?,
            AccountVisibility::parse(&self.visibility)?,
            AccountLifecycle::parse(&self.lifecycle)?,
            self.system_role
                .as_deref()
                .map(SystemAccountRole::parse)
                .transpose()?,
            AccountVersion::new(self.version)?,
            self.created_at,
            self.updated_at,
        )
    }
}
