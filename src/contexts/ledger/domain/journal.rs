//! Immutable balanced journal aggregate.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::shared_kernel::{
    CausationId, CorrelationId, CurrencyCode, IdempotencyKey, Money, UserId,
};

use super::{
    AccountNature, JournalEntryId, LedgerAccount, LedgerAccountId, LedgerError, PostingId,
    PostingPurpose,
};

/// Actor responsible for a recorded accounting fact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Actor {
    /// Authenticated user action.
    User(UserId),
    /// Moneykeeper policy or worker action.
    System,
    /// Provider-neutral external source.
    External {
        source_kind: String,
        source_reference: String,
    },
}

impl Serialize for Actor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        #[derive(Serialize)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        enum Wire<'a> {
            User {
                user_id: UserId,
            },
            System,
            External {
                source_kind: &'a str,
                source_reference: &'a str,
            },
        }
        match self {
            Self::User(user_id) => Wire::User { user_id: *user_id }.serialize(serializer),
            Self::System => Wire::System.serialize(serializer),
            Self::External {
                source_kind,
                source_reference,
            } => Wire::External {
                source_kind,
                source_reference,
            }
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Actor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        enum Wire {
            User {
                user_id: UserId,
            },
            System,
            External {
                source_kind: String,
                source_reference: String,
            },
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::User { user_id } => Self::User(user_id),
            Wire::System => Self::System,
            Wire::External {
                source_kind,
                source_reference,
            } => Self::External {
                source_kind,
                source_reference,
            },
        })
    }
}

/// Provider-neutral origin of a journal entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalSource {
    Manual,
    Import,
    System,
    Correction,
    Reconciliation,
}

impl JournalSource {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Import => "import",
            Self::System => "system",
            Self::Correction => "correction",
            Self::Reconciliation => "reconciliation",
        }
    }
}

/// Immutable links from a new journal to prior accounting facts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalRelations {
    reverses_transaction_id: Option<JournalEntryId>,
    corrects_transaction_id: Option<JournalEntryId>,
    replaces_transaction_id: Option<JournalEntryId>,
}

impl JournalRelations {
    /// Creates an unrelated journal.
    pub const fn none() -> Self {
        Self {
            reverses_transaction_id: None,
            corrects_transaction_id: None,
            replaces_transaction_id: None,
        }
    }

    /// Links an exact negating journal to its original.
    pub const fn reversal_of(id: JournalEntryId) -> Self {
        Self {
            reverses_transaction_id: Some(id),
            ..Self::none()
        }
    }

    /// Links a correction entry to an earlier entry.
    pub const fn correction_of(id: JournalEntryId) -> Self {
        Self {
            corrects_transaction_id: Some(id),
            ..Self::none()
        }
    }

    /// Links a replacement entry to the superseded original.
    pub const fn replacement_of(id: JournalEntryId) -> Self {
        Self {
            replaces_transaction_id: Some(id),
            ..Self::none()
        }
    }

    /// Returns the reversed journal identity.
    pub const fn reverses(self) -> Option<JournalEntryId> {
        self.reverses_transaction_id
    }

    /// Returns the corrected journal identity.
    pub const fn corrects(self) -> Option<JournalEntryId> {
        self.corrects_transaction_id
    }

    /// Returns the replaced journal identity.
    pub const fn replaces(self) -> Option<JournalEntryId> {
        self.replaces_transaction_id
    }

    fn validate(self, id: JournalEntryId, purpose: PostingPurpose) -> Result<(), LedgerError> {
        let relations = [self.reverses(), self.corrects(), self.replaces()];
        if relations.iter().flatten().any(|related| *related == id)
            || relations.iter().flatten().count() > 1
        {
            return Err(LedgerError::invalid_relation());
        }
        if purpose == PostingPurpose::Reversal && self.reverses().is_none() {
            return Err(LedgerError::invalid_relation());
        }
        Ok(())
    }
}

/// One immutable debit-positive/credit-negative posting owned by a journal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Posting {
    id: PostingId,
    position: u16,
    account_id: LedgerAccountId,
    user_id: UserId,
    currency: CurrencyCode,
    account_nature: AccountNature,
    signed_amount: Decimal,
}

impl Posting {
    /// Creates a posting from an account aggregate snapshot.
    ///
    /// The constructor makes account currency and tenant mismatches
    /// unrepresentable for normal callers.
    pub fn for_account(
        id: PostingId,
        account: &LedgerAccount,
        signed_amount: Decimal,
        purpose: PostingPurpose,
    ) -> Result<Self, LedgerError> {
        account.require_posting_allowed(purpose)?;
        if signed_amount.is_zero() {
            return Err(LedgerError::zero_posting());
        }
        Money::new(
            signed_amount,
            account.currency().clone(),
            Money::DATABASE_SCALE,
        )
        .map_err(|error| LedgerError::invalid_observation(error.to_string()))?;
        Ok(Self {
            id,
            position: 0,
            account_id: account.id(),
            user_id: account.user_id(),
            currency: account.currency().clone(),
            account_nature: account.nature(),
            signed_amount,
        })
    }

    pub(crate) fn rehydrate(
        id: PostingId,
        position: u16,
        account_id: LedgerAccountId,
        user_id: UserId,
        currency: CurrencyCode,
        account_nature: AccountNature,
        signed_amount: Decimal,
    ) -> Self {
        Self {
            id,
            position,
            account_id,
            user_id,
            currency,
            account_nature,
            signed_amount,
        }
    }

    /// Returns the immutable posting identity.
    pub const fn id(&self) -> PostingId {
        self.id
    }
    /// Returns the stable one-based order within the journal.
    pub const fn position(&self) -> u16 {
        self.position
    }
    /// Returns the referenced account identity.
    pub const fn account_id(&self) -> LedgerAccountId {
        self.account_id
    }
    /// Returns the owning tenant.
    pub const fn user_id(&self) -> UserId {
        self.user_id
    }
    /// Returns the posting currency.
    pub const fn currency(&self) -> &CurrencyCode {
        &self.currency
    }
    /// Returns the account nature snapshot used for display effects.
    pub const fn account_nature(&self) -> AccountNature {
        self.account_nature
    }
    /// Returns the raw debit-positive amount.
    pub const fn signed_amount(&self) -> Decimal {
        self.signed_amount
    }
    /// Returns the normalized display-balance effect.
    pub fn display_effect(&self) -> Decimal {
        self.signed_amount * Decimal::from(self.account_nature.normal_sign())
    }
}

/// Immutable journal-entry aggregate owning all postings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JournalEntry {
    id: JournalEntryId,
    user_id: UserId,
    description: String,
    purpose: PostingPurpose,
    source: JournalSource,
    actor: Actor,
    occurred_at: DateTime<Utc>,
    recorded_at: DateTime<Utc>,
    correlation_id: CorrelationId,
    causation_id: Option<CausationId>,
    idempotency_key: IdempotencyKey,
    relations: JournalRelations,
    fx_rate: Option<Decimal>,
    postings: Vec<Posting>,
}

impl JournalEntry {
    /// Posts one immutable journal after validating every aggregate invariant.
    #[allow(clippy::too_many_arguments)]
    pub fn post(
        id: JournalEntryId,
        user_id: UserId,
        description: impl Into<String>,
        purpose: PostingPurpose,
        source: JournalSource,
        actor: Actor,
        occurred_at: DateTime<Utc>,
        recorded_at: DateTime<Utc>,
        correlation_id: CorrelationId,
        causation_id: Option<CausationId>,
        idempotency_key: IdempotencyKey,
        relations: JournalRelations,
        mut postings: Vec<Posting>,
    ) -> Result<Self, LedgerError> {
        if postings.len() < 2 || postings.len() > usize::from(u16::MAX) {
            return Err(LedgerError::too_few_postings());
        }
        relations.validate(id, purpose)?;
        if matches!(actor, Actor::User(actor_id) if actor_id != user_id) {
            return Err(LedgerError::tenant_mismatch());
        }
        let description = validate_description(description.into())?;
        let mut totals = BTreeMap::<CurrencyCode, Decimal>::new();
        for (index, posting) in postings.iter_mut().enumerate() {
            if posting.user_id != user_id {
                return Err(LedgerError::tenant_mismatch());
            }
            if posting.signed_amount.is_zero() {
                return Err(LedgerError::zero_posting());
            }
            posting.position =
                u16::try_from(index + 1).map_err(|_| LedgerError::too_few_postings())?;
            let total = totals.entry(posting.currency.clone()).or_default();
            *total = total
                .checked_add(posting.signed_amount)
                .ok_or_else(LedgerError::unbalanced_journal)?;
        }
        if totals.values().any(|total| !total.is_zero()) {
            return Err(LedgerError::unbalanced_journal());
        }
        Ok(Self {
            id,
            user_id,
            description,
            purpose,
            source,
            actor,
            occurred_at,
            recorded_at,
            correlation_id,
            causation_id,
            idempotency_key,
            relations,
            fx_rate: None,
            postings,
        })
    }

    /// Consumes a newly built journal and records its supplied implied FX rate.
    pub fn with_fx_rate(mut self, implied_rate: Decimal) -> Result<Self, LedgerError> {
        if implied_rate <= Decimal::ZERO {
            return Err(LedgerError::invalid_money(
                "implied FX rate must be positive",
            ));
        }
        self.fx_rate = Some(implied_rate);
        Ok(self)
    }

    /// Builds the exact negating postings for a reversal command.
    pub fn reversing_postings(&self) -> Result<Vec<Posting>, LedgerError> {
        self.postings
            .iter()
            .map(|posting| {
                let signed_amount = -posting.signed_amount;
                Ok(Posting::rehydrate(
                    PostingId::generate(),
                    posting.position,
                    posting.account_id,
                    posting.user_id,
                    posting.currency.clone(),
                    posting.account_nature,
                    signed_amount,
                ))
            })
            .collect()
    }

    pub const fn id(&self) -> JournalEntryId {
        self.id
    }
    pub const fn user_id(&self) -> UserId {
        self.user_id
    }
    pub fn description(&self) -> &str {
        &self.description
    }
    pub const fn purpose(&self) -> PostingPurpose {
        self.purpose
    }
    pub const fn source(&self) -> JournalSource {
        self.source
    }
    pub const fn actor(&self) -> &Actor {
        &self.actor
    }
    pub const fn occurred_at(&self) -> DateTime<Utc> {
        self.occurred_at
    }
    pub const fn recorded_at(&self) -> DateTime<Utc> {
        self.recorded_at
    }
    pub const fn correlation_id(&self) -> CorrelationId {
        self.correlation_id
    }
    pub const fn causation_id(&self) -> Option<CausationId> {
        self.causation_id
    }
    pub fn idempotency_key(&self) -> &IdempotencyKey {
        &self.idempotency_key
    }
    pub const fn relations(&self) -> JournalRelations {
        self.relations
    }
    pub const fn fx_rate(&self) -> Option<Decimal> {
        self.fx_rate
    }
    pub fn postings(&self) -> &[Posting] {
        &self.postings
    }
}

fn validate_description(value: String) -> Result<String, LedgerError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 500 {
        return Err(LedgerError::invalid_annotation(
            "journal description must contain 1 to 500 characters",
        ));
    }
    Ok(value.to_owned())
}
