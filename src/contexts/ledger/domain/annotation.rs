//! Versioned transaction annotation aggregate.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::shared_kernel::UserId;

use super::{Actor, AnnotationId, JournalEntryId, LedgerError};

/// Ledger-owned reference to a category validated through Classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CategoryReference(Uuid);

impl CategoryReference {
    /// Creates a category reference from Classification's opaque UUID identity.
    pub const fn new(value: Uuid) -> Self { Self(value) }
    /// Returns the persisted UUID representation.
    pub const fn into_uuid(self) -> Uuid { self.0 }
}

/// Optimistic annotation version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AnnotationVersion(i64);

impl AnnotationVersion {
    pub const INITIAL: Self = Self(1);

    pub fn new(value: i64) -> Result<Self, LedgerError> {
        if value < 1 {
            return Err(LedgerError::invalid_version());
        }
        Ok(Self(value))
    }

    pub const fn get(self) -> i64 { self.0 }

    fn next(self) -> Result<Self, LedgerError> {
        self.0
            .checked_add(1)
            .ok_or_else(|| LedgerError::persistence("annotation version overflowed"))
            .and_then(Self::new)
    }
}

/// Explicit inclusion policy for budget/reporting consumers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetVisibility {
    Included,
    Excluded,
}

impl BudgetVisibility {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Included => "included",
            Self::Excluded => "excluded",
        }
    }
}

/// Bounded, normalized, sorted, duplicate-free transaction tags.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NormalizedTags(Vec<String>);

impl NormalizedTags {
    pub const MAX_TAGS: usize = 20;
    pub const MAX_TAG_CHARACTERS: usize = 40;

    /// Returns an empty tag set.
    pub const fn empty() -> Self { Self(Vec::new()) }

    /// Normalizes tag values by trimming and Unicode-lowercasing.
    pub fn new<I, S>(values: I) -> Result<Self, LedgerError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut normalized = BTreeSet::new();
        for value in values {
            let value = value.as_ref().trim().to_lowercase();
            if value.is_empty()
                || value.chars().count() > Self::MAX_TAG_CHARACTERS
                || value.chars().any(char::is_control)
            {
                return Err(LedgerError::invalid_tags());
            }
            normalized.insert(value);
        }
        if normalized.len() > Self::MAX_TAGS {
            return Err(LedgerError::invalid_tags());
        }
        Ok(Self(normalized.into_iter().collect()))
    }

    /// Returns the canonical tag order.
    pub fn as_slice(&self) -> &[String] { &self.0 }
}

/// Partial annotation mutation. Nested options distinguish unchanged from clear.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AnnotationChanges {
    pub description: Option<String>,
    pub category: Option<Option<CategoryReference>>,
    pub note: Option<Option<String>>,
    pub tags: Option<NormalizedTags>,
    pub budget_visibility: Option<BudgetVisibility>,
}

/// Immutable audit fact produced by an annotation mutation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnotationChanged {
    pub annotation_id: AnnotationId,
    pub journal_entry_id: JournalEntryId,
    pub user_id: UserId,
    pub version: AnnotationVersion,
    pub actor: Actor,
    pub changed_at: DateTime<Utc>,
}

/// Mutable metadata aggregate kept strictly separate from immutable postings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransactionAnnotation {
    id: AnnotationId,
    journal_entry_id: JournalEntryId,
    user_id: UserId,
    description: String,
    category: Option<CategoryReference>,
    note: Option<String>,
    tags: NormalizedTags,
    budget_visibility: BudgetVisibility,
    version: AnnotationVersion,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    audit_events: Vec<AnnotationChanged>,
}

impl TransactionAnnotation {
    /// Creates valid transaction metadata at version one.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: AnnotationId,
        journal_entry_id: JournalEntryId,
        user_id: UserId,
        description: impl Into<String>,
        category: Option<CategoryReference>,
        note: Option<String>,
        tags: NormalizedTags,
        budget_visibility: BudgetVisibility,
        now: DateTime<Utc>,
    ) -> Result<Self, LedgerError> {
        Ok(Self {
            id,
            journal_entry_id,
            user_id,
            description: validate_description(description.into())?,
            category,
            note: validate_note(note)?,
            tags,
            budget_visibility,
            version: AnnotationVersion::INITIAL,
            created_at: now,
            updated_at: now,
            audit_events: Vec::new(),
        })
    }

    /// Applies a compare-and-swap metadata edit and records its audit fact.
    pub fn update(
        &mut self,
        changes: AnnotationChanges,
        expected_version: AnnotationVersion,
        actor: Actor,
        now: DateTime<Utc>,
    ) -> Result<bool, LedgerError> {
        if self.version != expected_version {
            return Err(LedgerError::version_conflict());
        }
        if matches!(actor, Actor::User(actor_id) if actor_id != self.user_id) {
            return Err(LedgerError::tenant_mismatch());
        }

        let description = changes
            .description
            .map(validate_description)
            .transpose()?
            .unwrap_or_else(|| self.description.clone());
        let category = changes.category.unwrap_or(self.category);
        let note = changes.note.map(validate_note).transpose()?.unwrap_or_else(|| self.note.clone());
        let tags = changes.tags.unwrap_or_else(|| self.tags.clone());
        let budget_visibility = changes.budget_visibility.unwrap_or(self.budget_visibility);

        if description == self.description
            && category == self.category
            && note == self.note
            && tags == self.tags
            && budget_visibility == self.budget_visibility
        {
            return Ok(false);
        }

        self.description = description;
        self.category = category;
        self.note = note;
        self.tags = tags;
        self.budget_visibility = budget_visibility;
        self.version = self.version.next()?;
        self.updated_at = now;
        self.audit_events.push(AnnotationChanged {
            annotation_id: self.id,
            journal_entry_id: self.journal_entry_id,
            user_id: self.user_id,
            version: self.version,
            actor,
            changed_at: now,
        });
        Ok(true)
    }

    pub const fn id(&self) -> AnnotationId { self.id }
    pub const fn journal_entry_id(&self) -> JournalEntryId { self.journal_entry_id }
    pub const fn user_id(&self) -> UserId { self.user_id }
    pub fn description(&self) -> &str { &self.description }
    pub const fn category(&self) -> Option<CategoryReference> { self.category }
    pub fn note(&self) -> Option<&str> { self.note.as_deref() }
    pub const fn tags(&self) -> &NormalizedTags { &self.tags }
    pub const fn budget_visibility(&self) -> BudgetVisibility { self.budget_visibility }
    pub const fn version(&self) -> AnnotationVersion { self.version }
    pub const fn created_at(&self) -> DateTime<Utc> { self.created_at }
    pub const fn updated_at(&self) -> DateTime<Utc> { self.updated_at }
    pub fn audit_events(&self) -> &[AnnotationChanged] { &self.audit_events }
    pub(crate) fn take_audit_events(&mut self) -> Vec<AnnotationChanged> {
        std::mem::take(&mut self.audit_events)
    }
}

fn validate_description(value: String) -> Result<String, LedgerError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 500 {
        return Err(LedgerError::invalid_annotation(
            "annotation description must contain 1 to 500 characters",
        ));
    }
    Ok(value.to_owned())
}

fn validate_note(value: Option<String>) -> Result<Option<String>, LedgerError> {
    let Some(value) = value else { return Ok(None) };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.chars().count() > 2_000 {
        return Err(LedgerError::invalid_annotation(
            "annotation note cannot exceed 2000 characters",
        ));
    }
    Ok(Some(value.to_owned()))
}
