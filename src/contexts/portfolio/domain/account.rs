//! PortfolioAccount aggregate and lifecycle.

use super::PortfolioError;
use crate::shared_kernel::UserId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

crate::define_uuid_id!(#[doc = "Identifies a PortfolioAccount aggregate."] pub PortfolioAccountId);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountLifecycle {
    Active,
    Archived,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortfolioAccount {
    id: PortfolioAccountId,
    user_id: UserId,
    name: String,
    lifecycle: AccountLifecycle,
    version: u64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl PortfolioAccount {
    pub fn open(
        user_id: UserId,
        name: impl Into<String>,
        now: DateTime<Utc>,
    ) -> Result<Self, PortfolioError> {
        let name = valid_name(name)?;
        Ok(Self {
            id: PortfolioAccountId::generate(),
            user_id,
            name,
            lifecycle: AccountLifecycle::Active,
            version: 1,
            created_at: now,
            updated_at: now,
        })
    }
    pub fn rename(
        &mut self,
        name: impl Into<String>,
        expected_version: u64,
        now: DateTime<Utc>,
    ) -> Result<(), PortfolioError> {
        self.require_version(expected_version)?;
        self.name = valid_name(name)?;
        self.bump(now)
    }
    pub fn archive(
        &mut self,
        expected_version: u64,
        now: DateTime<Utc>,
    ) -> Result<(), PortfolioError> {
        self.require_version(expected_version)?;
        if self.lifecycle != AccountLifecycle::Active {
            return Err(PortfolioError::InvalidValue("lifecycle"));
        }
        self.lifecycle = AccountLifecycle::Archived;
        self.bump(now)
    }
    pub fn restore(
        &mut self,
        expected_version: u64,
        now: DateTime<Utc>,
    ) -> Result<(), PortfolioError> {
        self.require_version(expected_version)?;
        if self.lifecycle != AccountLifecycle::Archived {
            return Err(PortfolioError::InvalidValue("lifecycle"));
        }
        self.lifecycle = AccountLifecycle::Active;
        self.bump(now)
    }
    pub fn require_activity_allowed(&self, reversal: bool) -> Result<(), PortfolioError> {
        if self.lifecycle == AccountLifecycle::Archived && !reversal {
            return Err(PortfolioError::AccountArchived);
        }
        Ok(())
    }
    fn require_version(&self, expected: u64) -> Result<(), PortfolioError> {
        if self.version != expected {
            return Err(PortfolioError::VersionConflict);
        }
        Ok(())
    }
    fn bump(&mut self, now: DateTime<Utc>) -> Result<(), PortfolioError> {
        self.version = self
            .version
            .checked_add(1)
            .ok_or(PortfolioError::Arithmetic)?;
        self.updated_at = now;
        Ok(())
    }
    pub const fn id(&self) -> PortfolioAccountId {
        self.id
    }
    pub const fn user_id(&self) -> UserId {
        self.user_id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub const fn lifecycle(&self) -> AccountLifecycle {
        self.lifecycle
    }
    pub const fn version(&self) -> u64 {
        self.version
    }
    pub const fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
    pub const fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }
}

fn valid_name(name: impl Into<String>) -> Result<String, PortfolioError> {
    let name = name.into();
    if name.is_empty() || name.trim() != name || name.chars().any(char::is_control) {
        return Err(PortfolioError::InvalidValue("name"));
    }
    Ok(name)
}
