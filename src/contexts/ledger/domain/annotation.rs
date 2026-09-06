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
    pub const fn new(value: Uuid) -> Self {
        Self(value)
    }
    /// Returns the persisted UUID representation.
    pub const fn into_uuid(self) -> Uuid {
        self.0
    }
}

/// Provenance of the category currently attached to a transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssignmentOrigin {
    Manual,
    Recurring,
    Ai,
}

impl AssignmentOrigin {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Recurring => "recurring",
            Self::Ai => "ai",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, LedgerError> {
        match value {
            "manual" => Ok(Self::Manual),
            "recurring" => Ok(Self::Recurring),
            "ai" => Ok(Self::Ai),
            _ => Err(LedgerError::persistence(
                "stored category assignment origin is invalid",
            )),
        }
    }
}

/// Whether automatic classification may currently change an uncategorized transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationState {
    Eligible,
    Suppressed,
    LegacyUnknown,
}

impl AutomationState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Eligible => "eligible",
            Self::Suppressed => "suppressed",
            Self::LegacyUnknown => "legacy_unknown",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, LedgerError> {
        match value {
            "eligible" => Ok(Self::Eligible),
            "suppressed" => Ok(Self::Suppressed),
            "legacy_unknown" => Ok(Self::LegacyUnknown),
            _ => Err(LedgerError::persistence(
                "stored automatic classification state is invalid",
            )),
        }
    }
}

/// Complete category-assignment state captured for restart-safe compensation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryAssignmentSnapshot {
    pub category: Option<CategoryReference>,
    pub origin: Option<AssignmentOrigin>,
    pub classification_decision_id: Option<Uuid>,
    pub automation_state: AutomationState,
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

    pub const fn get(self) -> i64 {
        self.0
    }

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
    pub const fn empty() -> Self {
        Self(Vec::new())
    }

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
    pub fn as_slice(&self) -> &[String] {
        &self.0
    }
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
    assignment_origin: Option<AssignmentOrigin>,
    classification_decision_id: Option<Uuid>,
    automation_state: AutomationState,
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
        let (assignment_origin, automation_state) = if category.is_some() {
            (Some(AssignmentOrigin::Manual), AutomationState::Suppressed)
        } else {
            (None, AutomationState::Eligible)
        };
        Ok(Self {
            id,
            journal_entry_id,
            user_id,
            description: validate_description(description.into())?,
            category,
            assignment_origin,
            classification_decision_id: None,
            automation_state,
            note: validate_note(note)?,
            tags,
            budget_visibility,
            version: AnnotationVersion::INITIAL,
            created_at: now,
            updated_at: now,
            audit_events: Vec::new(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn rehydrate(
        id: AnnotationId,
        journal_entry_id: JournalEntryId,
        user_id: UserId,
        description: String,
        category: Option<CategoryReference>,
        assignment_origin: Option<AssignmentOrigin>,
        classification_decision_id: Option<Uuid>,
        automation_state: AutomationState,
        note: Option<String>,
        tags: NormalizedTags,
        budget_visibility: BudgetVisibility,
        version: AnnotationVersion,
        created_at: DateTime<Utc>,
        updated_at: DateTime<Utc>,
    ) -> Result<Self, LedgerError> {
        let assignment_is_valid = match assignment_origin {
            None => category.is_none() && classification_decision_id.is_none(),
            Some(AssignmentOrigin::Manual) => automation_state == AutomationState::Suppressed,
            Some(AssignmentOrigin::Recurring) => {
                category.is_some()
                    && classification_decision_id.is_none()
                    && automation_state == AutomationState::Eligible
            }
            Some(AssignmentOrigin::Ai) => {
                category.is_some()
                    && classification_decision_id.is_some()
                    && automation_state == AutomationState::Eligible
            }
        };
        if !assignment_is_valid {
            return Err(LedgerError::persistence(
                "stored category assignment provenance is invalid",
            ));
        }
        Ok(Self {
            id,
            journal_entry_id,
            user_id,
            description: validate_description(description)?,
            category,
            assignment_origin,
            classification_decision_id,
            automation_state,
            note: validate_note(note)?,
            tags,
            budget_visibility,
            version,
            created_at,
            updated_at,
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

        let category_touched = changes.category.is_some();
        let description = changes
            .description
            .map(validate_description)
            .transpose()?
            .unwrap_or_else(|| self.description.clone());
        let category = changes.category.unwrap_or(self.category);
        let assignment_origin = if category_touched {
            Some(AssignmentOrigin::Manual)
        } else {
            self.assignment_origin
        };
        let classification_decision_id = if category_touched {
            None
        } else {
            self.classification_decision_id
        };
        let automation_state = if category_touched {
            AutomationState::Suppressed
        } else {
            self.automation_state
        };
        let note = changes
            .note
            .map(validate_note)
            .transpose()?
            .unwrap_or_else(|| self.note.clone());
        let tags = changes.tags.unwrap_or_else(|| self.tags.clone());
        let budget_visibility = changes.budget_visibility.unwrap_or(self.budget_visibility);

        if description == self.description
            && category == self.category
            && assignment_origin == self.assignment_origin
            && classification_decision_id == self.classification_decision_id
            && automation_state == self.automation_state
            && note == self.note
            && tags == self.tags
            && budget_visibility == self.budget_visibility
        {
            return Ok(false);
        }

        self.description = description;
        self.category = category;
        self.assignment_origin = assignment_origin;
        self.classification_decision_id = classification_decision_id;
        self.automation_state = automation_state;
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

    /// Applies a version-fenced system assignment while enforcing Manual > Recurring > AI.
    pub fn apply_system_assignment(
        &mut self,
        category: Option<CategoryReference>,
        origin: AssignmentOrigin,
        classification_decision_id: Option<Uuid>,
        expected_version: AnnotationVersion,
        now: DateTime<Utc>,
    ) -> Result<bool, LedgerError> {
        if self.version != expected_version {
            return Err(LedgerError::version_conflict());
        }
        if origin == AssignmentOrigin::Ai
            && (classification_decision_id.is_none() || category.is_none())
        {
            return Err(LedgerError::invalid_annotation(
                "AI assignments require a category and classification decision",
            ));
        }
        if origin == AssignmentOrigin::Recurring
            && (classification_decision_id.is_some() || category.is_none())
        {
            return Err(LedgerError::invalid_annotation(
                "Recurring assignments require a category without an AI decision",
            ));
        }
        if origin == AssignmentOrigin::Manual && classification_decision_id.is_none() {
            return Err(LedgerError::invalid_annotation(
                "system-applied manual assignments require a classification decision",
            ));
        }
        if origin != AssignmentOrigin::Manual
            && (self.automation_state == AutomationState::Suppressed
                || self.assignment_origin == Some(AssignmentOrigin::Manual))
        {
            return Ok(false);
        }
        if origin == AssignmentOrigin::Ai
            && (self.category.is_some() || self.automation_state != AutomationState::Eligible)
        {
            return Ok(false);
        }
        if origin == AssignmentOrigin::Recurring
            && self.assignment_origin == Some(AssignmentOrigin::Recurring)
            && self.category == category
        {
            return Ok(false);
        }

        self.category = category;
        self.assignment_origin = Some(origin);
        self.classification_decision_id =
            if matches!(origin, AssignmentOrigin::Ai | AssignmentOrigin::Manual) {
                classification_decision_id
            } else {
                None
            };
        self.automation_state = if origin == AssignmentOrigin::Manual {
            AutomationState::Suppressed
        } else {
            AutomationState::Eligible
        };
        self.record_change(Actor::System, now)?;
        Ok(true)
    }

    /// Restores a complete pre-Recurring assignment snapshot during compensation.
    pub fn restore_assignment(
        &mut self,
        snapshot: CategoryAssignmentSnapshot,
        expected_version: AnnotationVersion,
        now: DateTime<Utc>,
    ) -> Result<bool, LedgerError> {
        if self.version != expected_version {
            return Err(LedgerError::version_conflict());
        }
        if self.assignment_origin != Some(AssignmentOrigin::Recurring) {
            return Ok(false);
        }
        if self.assignment_snapshot() == snapshot {
            return Ok(false);
        }
        self.category = snapshot.category;
        self.assignment_origin = snapshot.origin;
        self.classification_decision_id = snapshot.classification_decision_id;
        self.automation_state = snapshot.automation_state;
        self.record_change(Actor::System, now)?;
        Ok(true)
    }

    /// Re-enables AI classification after an explicit user retry.
    pub fn enable_automatic_classification(
        &mut self,
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
        if self.category.is_some() {
            return Err(LedgerError::invalid_annotation(
                "automatic classification can only be retried while uncategorized",
            ));
        }
        // An explicit retry creates a new generation even after an eligible
        // transaction previously abstained or exhausted provider retries.
        self.assignment_origin = None;
        self.classification_decision_id = None;
        self.automation_state = AutomationState::Eligible;
        self.record_change(actor, now)?;
        Ok(true)
    }

    fn record_change(&mut self, actor: Actor, now: DateTime<Utc>) -> Result<(), LedgerError> {
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
        Ok(())
    }

    pub const fn id(&self) -> AnnotationId {
        self.id
    }
    pub const fn journal_entry_id(&self) -> JournalEntryId {
        self.journal_entry_id
    }
    pub const fn user_id(&self) -> UserId {
        self.user_id
    }
    pub fn description(&self) -> &str {
        &self.description
    }
    pub const fn category(&self) -> Option<CategoryReference> {
        self.category
    }
    pub const fn assignment_origin(&self) -> Option<AssignmentOrigin> {
        self.assignment_origin
    }
    pub const fn classification_decision_id(&self) -> Option<Uuid> {
        self.classification_decision_id
    }
    pub const fn automation_state(&self) -> AutomationState {
        self.automation_state
    }
    pub const fn assignment_snapshot(&self) -> CategoryAssignmentSnapshot {
        CategoryAssignmentSnapshot {
            category: self.category,
            origin: self.assignment_origin,
            classification_decision_id: self.classification_decision_id,
            automation_state: self.automation_state,
        }
    }
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }
    pub const fn tags(&self) -> &NormalizedTags {
        &self.tags
    }
    pub const fn budget_visibility(&self) -> BudgetVisibility {
        self.budget_visibility
    }
    pub const fn version(&self) -> AnnotationVersion {
        self.version
    }
    pub const fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
    pub const fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }
    pub fn audit_events(&self) -> &[AnnotationChanged] {
        &self.audit_events
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
