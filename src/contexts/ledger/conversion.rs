//! Transfer conversion contracts: exact movements, immutable sources and lifecycle.
use super::public::{JournalEntryId, LedgerAccountId, LedgerError};
use crate::shared_kernel::{CurrencyCode, IdempotencyKey};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ConversionMoney {
    #[serde(with = "rust_decimal::serde::str")]
    pub amount: Decimal,
    pub currency: CurrencyCode,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConversionInput {
    pub other_account_id: LedgerAccountId,
    pub other_journal_id: Option<JournalEntryId>,
    pub missing_side: Option<ConversionMoney>,
    pub fee: Option<ConversionMoney>,
    #[serde(default)]
    pub confirm_fee: bool,
    pub occurred_at: Option<DateTime<Utc>>,
    pub title: String,
    pub note: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConversionMovement {
    pub account_id: LedgerAccountId,
    pub account_name: String,
    pub money: ConversionMoney,
    #[serde(with = "rust_decimal::serde::str")]
    pub signed_amount: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub balance_change: Decimal,
    pub balance_version: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConversionPreview {
    pub version_token: String,
    pub outgoing: ConversionMovement,
    pub incoming: ConversionMovement,
    pub source_principal: ConversionMoney,
    pub target_principal: ConversionMoney,
    pub fee: Option<ConversionMoney>,
    pub source_per_target_rate: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub removed_from_totals: Vec<ConversionMoney>,
    pub eligibility_issues: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConversionSource {
    pub journal_id: JournalEntryId,
    pub description: String,
    pub occurred_at: DateTime<Utc>,
    pub annotation: Option<serde_json::Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConversionLifecycle {
    pub action: String,
    pub at: DateTime<Utc>,
    pub journal_ids: Vec<JournalEntryId>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransferConversion {
    pub id: Uuid,
    pub version: i64,
    pub active: bool,
    pub title: String,
    pub note: Option<String>,
    pub preview: ConversionPreview,
    pub sources: Vec<ConversionSource>,
    pub transfer_journal_id: JournalEntryId,
    pub generated_journal_ids: Vec<JournalEntryId>,
    pub restorations: Vec<(JournalEntryId, JournalEntryId)>,
    pub lifecycle: Vec<ConversionLifecycle>,
}
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ConversionCandidatesQuery {
    pub other_account_id: Option<LedgerAccountId>,
    pub search: Option<String>,
    pub offset: Option<u32>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConversionCandidate {
    pub journal_id: JournalEntryId,
    pub account_id: LedgerAccountId,
    pub description: String,
    pub occurred_at: DateTime<Utc>,
    pub money: ConversionMoney,
}
/// Normalize principal without ever changing the two recorded movements.
pub(crate) fn principals(
    out: &ConversionMoney,
    incoming: &ConversionMoney,
    fee: Option<&ConversionMoney>,
    confirmed: bool,
) -> Result<(ConversionMoney, ConversionMoney), LedgerError> {
    let mut source = out.clone();
    let mut target = incoming.clone();
    if source.amount <= Decimal::ZERO || target.amount <= Decimal::ZERO {
        return Err(LedgerError::invalid_money(
            "transfer movements must be positive",
        ));
    }
    if let Some(fee) = fee {
        if fee.amount <= Decimal::ZERO {
            return Err(LedgerError::invalid_money("fee must be positive"));
        }
        if fee.currency == source.currency {
            source.amount = source
                .amount
                .checked_sub(fee.amount)
                .ok_or_else(LedgerError::unbalanced_journal)?;
        } else if fee.currency == target.currency {
            target.amount = target
                .amount
                .checked_add(fee.amount)
                .ok_or_else(LedgerError::unbalanced_journal)?;
        } else {
            return Err(LedgerError::currency_mismatch());
        }
    }
    if source.amount <= Decimal::ZERO {
        return Err(LedgerError::invalid_money("fee consumes outgoing movement"));
    }
    if source.currency == target.currency
        && (source.amount != target.amount || (fee.is_some() && !confirmed))
    {
        return Err(LedgerError::invalid_money(
            "same-currency difference requires an explicit confirmed fee; incoming cannot exceed outgoing",
        ));
    }
    Ok((source, target))
}
#[derive(Clone, Debug)]
pub enum ConversionAction {
    AttachmentCandidates {
        id: Uuid,
        query: ConversionCandidatesQuery,
    },
    Notifications,
    Attach {
        id: Uuid,
        journal_id: JournalEntryId,
        expected_version: i64,
        key: IdempotencyKey,
    },
    Reviews {
        id: Option<Uuid>,
    },
    ResolveReview {
        id: Uuid,
        conversion_id: Option<Uuid>,
        expected_version: i64,
        key: IdempotencyKey,
    },
    ResolveProvider {
        previous: Option<JournalEntryId>,
        stream: String,
        item: String,
        changed: bool,
    },

    Candidates {
        journal_id: JournalEntryId,
        query: ConversionCandidatesQuery,
    },
    Preview {
        journal_id: JournalEntryId,
        input: ConversionInput,
    },
    Convert {
        journal_id: JournalEntryId,
        input: ConversionInput,
        version_token: String,
        key: IdempotencyKey,
    },
    Get {
        id: Uuid,
    },
    Edit {
        id: Uuid,
        expected_version: i64,
        title: String,
        note: Option<String>,
    },
    Undo {
        id: Uuid,
        expected_version: i64,
        key: IdempotencyKey,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConversionResponse {
    Notifications(Vec<TransferConversion>),
    Reviews(Vec<ConversionImportReview>),
    Reference { journal_id: Option<JournalEntryId> },

    Preview(ConversionPreview),
    Conversion(TransferConversion),
    Candidates(Vec<ConversionCandidate>),
}

/// Provider-neutral reference and exact movement held for user review. Raw evidence stays in Banking.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConversionImportReview {
    pub id: Uuid,
    pub version: i64,
    pub state: String,
    pub stream: String,
    pub item: String,
    pub account_id: LedgerAccountId,
    pub money: ConversionMoney,
    pub description: String,
    pub occurred_at: DateTime<Utc>,
    pub candidates: Vec<Uuid>,
    pub journal_id: Option<JournalEntryId>,
    pub conversion_id: Option<Uuid>,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn money(amount: i64, currency: &str) -> ConversionMoney {
        ConversionMoney {
            amount: Decimal::from(amount),
            currency: CurrencyCode::new(currency).unwrap(),
        }
    }
    #[test]
    fn same_currency_difference_requires_exact_confirmed_fee() {
        assert!(principals(&money(102, "UAH"), &money(100, "UAH"), None, true).is_err());
        assert!(
            principals(
                &money(102, "UAH"),
                &money(100, "UAH"),
                Some(&money(2, "UAH")),
                false
            )
            .is_err()
        );
        assert_eq!(
            principals(
                &money(102, "UAH"),
                &money(100, "UAH"),
                Some(&money(2, "UAH")),
                true
            )
            .unwrap(),
            (money(100, "UAH"), money(100, "UAH"))
        );
        assert!(principals(&money(100, "UAH"), &money(102, "UAH"), None, true).is_err());
    }
    #[test]
    fn fx_fees_adjust_principal_in_the_represented_currency_only() {
        assert_eq!(
            principals(
                &money(4020, "UAH"),
                &money(100, "USD"),
                Some(&money(20, "UAH")),
                false
            )
            .unwrap(),
            (money(4000, "UAH"), money(100, "USD"))
        );
        assert_eq!(
            principals(
                &money(4000, "UAH"),
                &money(99, "USD"),
                Some(&money(1, "USD")),
                false
            )
            .unwrap(),
            (money(4000, "UAH"), money(100, "USD"))
        );
        assert!(
            principals(
                &money(4000, "UAH"),
                &money(100, "USD"),
                Some(&money(1, "EUR")),
                true
            )
            .is_err()
        );
        assert!(
            principals(
                &money(4000, "UAH"),
                &money(100, "USD"),
                Some(&money(4000, "UAH")),
                true
            )
            .is_err()
        );
    }
}
