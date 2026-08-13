use chrono::{DateTime, Utc};

use crate::shared_kernel::UserId;

use super::public::{CategoryId, CategoryKind, CategoryLifecycle, ClassificationError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Category {
    id: CategoryId,
    user_id: UserId,
    name: String,
    kind: CategoryKind,
    lifecycle: CategoryLifecycle,
    version: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl Category {
    pub(crate) fn create(
        id: CategoryId,
        user_id: UserId,
        name: String,
        kind: CategoryKind,
        now: DateTime<Utc>,
    ) -> Result<Self, ClassificationError> {
        let name = validate_name(name)?;
        Ok(Self {
            id,
            user_id,
            name,
            kind,
            lifecycle: CategoryLifecycle::Active,
            version: 1,
            created_at: now,
            updated_at: now,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn reconstitute(
        id: CategoryId,
        user_id: UserId,
        name: String,
        kind: CategoryKind,
        lifecycle: CategoryLifecycle,
        version: i64,
        created_at: DateTime<Utc>,
        updated_at: DateTime<Utc>,
    ) -> Result<Self, ClassificationError> {
        if version < 1 {
            return Err(ClassificationError::persistence(
                "stored category version is invalid",
            ));
        }
        Ok(Self {
            id,
            user_id,
            name: validate_name(name)?,
            kind,
            lifecycle,
            version,
            created_at,
            updated_at,
        })
    }

    pub(crate) fn rename(
        &mut self,
        name: String,
        expected_version: i64,
        now: DateTime<Utc>,
    ) -> Result<bool, ClassificationError> {
        self.require_version(expected_version)?;
        let name = validate_name(name)?;
        if self.name == name {
            return Ok(false);
        }
        self.name = name;
        self.bump(now);
        Ok(true)
    }

    pub(crate) fn archive(
        &mut self,
        expected_version: i64,
        now: DateTime<Utc>,
    ) -> Result<bool, ClassificationError> {
        self.require_version(expected_version)?;
        if self.lifecycle == CategoryLifecycle::Archived {
            return Ok(false);
        }
        self.lifecycle = CategoryLifecycle::Archived;
        self.bump(now);
        Ok(true)
    }

    pub(crate) fn restore(
        &mut self,
        expected_version: i64,
        now: DateTime<Utc>,
    ) -> Result<bool, ClassificationError> {
        self.require_version(expected_version)?;
        if self.lifecycle == CategoryLifecycle::Active {
            return Ok(false);
        }
        self.lifecycle = CategoryLifecycle::Active;
        self.bump(now);
        Ok(true)
    }

    fn require_version(&self, expected_version: i64) -> Result<(), ClassificationError> {
        if expected_version != self.version {
            return Err(ClassificationError::version_conflict());
        }
        Ok(())
    }

    fn bump(&mut self, now: DateTime<Utc>) {
        self.version += 1;
        self.updated_at = now;
    }

    pub(crate) fn id(&self) -> CategoryId {
        self.id
    }

    pub(crate) fn user_id(&self) -> UserId {
        self.user_id
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn kind(&self) -> CategoryKind {
        self.kind
    }

    pub(crate) fn lifecycle(&self) -> CategoryLifecycle {
        self.lifecycle
    }

    pub(crate) fn version(&self) -> i64 {
        self.version
    }

    pub(crate) fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    pub(crate) fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }
}

fn validate_name(name: String) -> Result<String, ClassificationError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ClassificationError::invalid_name());
    }
    if name.chars().count() > 100 {
        return Err(ClassificationError::invalid_name());
    }
    Ok(name.to_owned())
}
