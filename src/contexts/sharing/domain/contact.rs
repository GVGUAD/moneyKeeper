//! External Contact aggregate.

use super::SharingError;
use crate::{define_uuid_id, shared_kernel::UserId};

define_uuid_id!(
    /// Identifies a user-owned external contact.
    pub ContactId
);

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct ContactVersion(pub u64);

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct ContactName(String);

impl ContactName {
    pub fn new(value: impl AsRef<str>) -> Result<Self, SharingError> {
        let normalized = value
            .as_ref()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if normalized.is_empty() {
            return Err(SharingError::Empty("contact name"));
        }
        if normalized.len() > 200 {
            return Err(SharingError::TooLong("contact name"));
        }
        Ok(Self(normalized))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ContactName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContactStatus {
    Active,
    Archived,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Contact {
    id: ContactId,
    user_id: UserId,
    name: ContactName,
    note: Option<String>,
    status: ContactStatus,
    version: ContactVersion,
}

impl Contact {
    pub fn create(
        id: ContactId,
        user_id: UserId,
        name: ContactName,
        note: Option<String>,
    ) -> Result<Self, SharingError> {
        Ok(Self {
            id,
            user_id,
            name,
            note: normalize_note(note)?,
            status: ContactStatus::Active,
            version: ContactVersion(1),
        })
    }
    pub fn rehydrate(
        id: ContactId,
        user_id: UserId,
        name: ContactName,
        note: Option<String>,
        status: ContactStatus,
        version: ContactVersion,
    ) -> Self {
        Self {
            id,
            user_id,
            name,
            note,
            status,
            version,
        }
    }
    pub fn edit(
        &mut self,
        name: ContactName,
        note: Option<String>,
        expected: ContactVersion,
    ) -> Result<(), SharingError> {
        self.require_version(expected)?;
        self.name = name;
        self.note = normalize_note(note)?;
        self.version.0 += 1;
        Ok(())
    }
    pub fn archive(&mut self, expected: ContactVersion) -> Result<(), SharingError> {
        self.require_version(expected)?;
        if self.status != ContactStatus::Active {
            return Err(SharingError::InvalidTransition);
        }
        self.status = ContactStatus::Archived;
        self.version.0 += 1;
        Ok(())
    }
    pub fn restore(&mut self, expected: ContactVersion) -> Result<(), SharingError> {
        self.require_version(expected)?;
        if self.status != ContactStatus::Archived {
            return Err(SharingError::InvalidTransition);
        }
        self.status = ContactStatus::Active;
        self.version.0 += 1;
        Ok(())
    }
    pub fn ensure_selectable(&self) -> Result<(), SharingError> {
        if self.status == ContactStatus::Archived {
            Err(SharingError::ContactArchived)
        } else {
            Ok(())
        }
    }
    fn require_version(&self, expected: ContactVersion) -> Result<(), SharingError> {
        if expected == self.version {
            Ok(())
        } else {
            Err(SharingError::VersionConflict {
                expected: expected.0,
                actual: self.version.0,
            })
        }
    }
    pub const fn id(&self) -> ContactId {
        self.id
    }
    pub const fn user_id(&self) -> UserId {
        self.user_id
    }
    pub fn name(&self) -> &ContactName {
        &self.name
    }
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }
    pub const fn status(&self) -> ContactStatus {
        self.status
    }
    pub const fn version(&self) -> ContactVersion {
        self.version
    }
}

fn normalize_note(note: Option<String>) -> Result<Option<String>, SharingError> {
    note.map(|value| {
        let value = value.trim().to_owned();
        if value.len() > 2_000 {
            return Err(SharingError::TooLong("contact note"));
        }
        Ok((!value.is_empty()).then_some(value))
    })
    .transpose()
    .map(Option::flatten)
}
