//! Transaction-bound transfer conversion persistence.
use super::pg_unit_of_work::PgLedgerTransaction;
use crate::contexts::ledger::{application::ports::ConversionStore, public::*};
use crate::shared_kernel::{CurrencyCode, UserId};
use async_trait::async_trait;
use sqlx::Row;

#[async_trait]
impl ConversionStore for PgLedgerTransaction<'_> {
    async fn conversion_notifications(
        &mut self,
        user: UserId,
    ) -> Result<Vec<TransferConversion>, LedgerError> {
        let docs:Vec<serde_json::Value>=sqlx::query_scalar("SELECT document FROM ledger.transfer_conversions WHERE user_id=$1 AND document->'lifecycle' @> '[{\"action\":\"bank_revision_automatically_undid_conversion\"}]'::jsonb ORDER BY id").bind(user.into_uuid()).fetch_all(&mut *self.transaction).await.map_err(LedgerError::storage)?;
        docs.into_iter()
            .map(|v| serde_json::from_value(v).map_err(|e| LedgerError::persistence(e.to_string())))
            .collect()
    }

    async fn hold_conversion_import(
        &mut self,
        user: UserId,
        mut review: ConversionImportReview,
    ) -> Result<Option<ConversionImportReview>, LedgerError> {
        let existing: Option<serde_json::Value>=sqlx::query_scalar("SELECT document FROM ledger.conversion_import_reviews WHERE user_id=$1 AND id=$2 FOR UPDATE").bind(user.into_uuid()).bind(review.id).fetch_optional(&mut *self.transaction).await.map_err(LedgerError::storage)?;
        if let Some(v) = existing {
            let existing: ConversionImportReview =
                serde_json::from_value(v).map_err(|e| LedgerError::persistence(e.to_string()))?;
            if existing.stream != review.stream
                || existing.item != review.item
                || existing.account_id != review.account_id
                || existing.money != review.money
                || existing.description != review.description
                || existing.occurred_at != review.occurred_at
            {
                return Err(LedgerError::idempotency_conflict());
            }
            return Ok(Some(existing));
        }
        let docs:Vec<serde_json::Value>=sqlx::query_scalar("SELECT document FROM ledger.transfer_conversions WHERE user_id=$1 AND active FOR UPDATE").bind(user.into_uuid()).fetch_all(&mut *self.transaction).await.map_err(LedgerError::storage)?;
        for doc in docs {
            let c: TransferConversion =
                serde_json::from_value(doc).map_err(|e| LedgerError::persistence(e.to_string()))?;
            if c.sources.len() != 1 {
                continue;
            }
            let original = super::super::application::ports::JournalStore::find_journal(
                self,
                user,
                c.sources[0].journal_id,
                false,
            )
            .await?
            .ok_or_else(LedgerError::not_found)?;
            if original
                .postings
                .iter()
                .any(|p| p.account_id() == review.account_id)
            {
                continue;
            }
            if [&c.preview.outgoing, &c.preview.incoming].iter().any(|m| {
                m.account_id == review.account_id
                    && m.money.currency == review.money.currency
                    && m.signed_amount == review.money.amount
            }) && (c.preview.occurred_at - review.occurred_at)
                .num_seconds()
                .abs()
                <= 604800
            {
                review.candidates.push(c.id);
            }
        }
        if review.candidates.is_empty() {
            return Ok(None);
        }
        self.save_conversion_review(user, &review).await?;
        Ok(Some(review))
    }
    async fn conversion_reviews(
        &mut self,
        user: UserId,
        id: Option<uuid::Uuid>,
    ) -> Result<Vec<ConversionImportReview>, LedgerError> {
        let docs:Vec<serde_json::Value>=sqlx::query_scalar("SELECT document FROM ledger.conversion_import_reviews WHERE user_id=$1 AND ($2::uuid IS NULL OR id=$2) ORDER BY id FOR UPDATE").bind(user.into_uuid()).bind(id).fetch_all(&mut *self.transaction).await.map_err(LedgerError::storage)?;
        docs.into_iter()
            .map(|v| serde_json::from_value(v).map_err(|e| LedgerError::persistence(e.to_string())))
            .collect()
    }
    async fn save_conversion_review(
        &mut self,
        user: UserId,
        r: &ConversionImportReview,
    ) -> Result<(), LedgerError> {
        sqlx::query("INSERT INTO ledger.conversion_import_reviews(user_id,id,version,state,document) VALUES($1,$2,$3,$4,$5) ON CONFLICT(user_id,id) DO UPDATE SET version=EXCLUDED.version,state=EXCLUDED.state,document=EXCLUDED.document").bind(user.into_uuid()).bind(r.id).bind(r.version).bind(&r.state).bind(serde_json::to_value(r).map_err(|e|LedgerError::persistence(e.to_string()))?).execute(&mut *self.transaction).await.map_err(LedgerError::storage)?;
        Ok(())
    }
    async fn resolve_conversion_reference(
        &mut self,
        user: UserId,
        mut journal: JournalEntryId,
    ) -> Result<JournalEntryId, LedgerError> {
        let mut seen = std::collections::BTreeSet::new();
        while seen.insert(journal) {
            let next:Option<uuid::Uuid>=sqlx::query_scalar("SELECT restored_id FROM ledger.transfer_restorations WHERE user_id=$1 AND original_id=$2").bind(user.into_uuid()).bind(journal.into_uuid()).fetch_optional(&mut *self.transaction).await.map_err(LedgerError::storage)?;
            match next {
                Some(next) => journal = JournalEntryId::new(next),
                None => return Ok(journal),
            }
        }
        Err(LedgerError::invalid_state("restoration cycle"))
    }
    async fn managed_conversion(
        &mut self,
        user: UserId,
        journal: JournalEntryId,
    ) -> Result<Option<uuid::Uuid>, LedgerError> {
        sqlx::query_scalar("SELECT c.id FROM ledger.transfer_conversions c JOIN ledger.transfer_conversion_journals j ON j.user_id=c.user_id AND j.conversion_id=c.id WHERE j.user_id=$1 AND j.journal_id=$2 AND c.active AND j.role<>'restoration'").bind(user.into_uuid()).bind(journal.into_uuid()).fetch_optional(&mut *self.transaction).await.map_err(LedgerError::storage)
    }

    async fn lock_conversion_scope(&mut self, user: UserId) -> Result<(), LedgerError> {
        // All conversion and competing journal operations acquire this before row locks.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 20420))")
            .bind(user.to_string())
            .execute(&mut *self.transaction)
            .await
            .map_err(LedgerError::storage)?;
        Ok(())
    }
    async fn conversion_source(
        &mut self,
        user: UserId,
        id: JournalEntryId,
    ) -> Result<ConversionSource, LedgerError> {
        let row = sqlx::query("SELECT j.description,j.occurred_at,to_jsonb(a) AS annotation FROM ledger.journal_entries j LEFT JOIN ledger.transaction_annotations a ON a.user_id=j.user_id AND a.journal_entry_id=j.id WHERE j.user_id=$1 AND j.id=$2")
            .bind(user.into_uuid()).bind(id.into_uuid()).fetch_optional(&mut *self.transaction).await.map_err(LedgerError::storage)?.ok_or_else(LedgerError::not_found)?;
        Ok(ConversionSource {
            journal_id: id,
            description: row.get("description"),
            occurred_at: row.get("occurred_at"),
            annotation: row.get("annotation"),
        })
    }
    async fn require_unclaimed(
        &mut self,
        user: UserId,
        id: JournalEntryId,
    ) -> Result<(), LedgerError> {
        let claimed: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM ledger.transfer_conversion_journals WHERE user_id=$1 AND journal_id=$2 AND role<>'restoration')")
            .bind(user.into_uuid()).bind(id.into_uuid()).fetch_one(&mut *self.transaction).await.map_err(LedgerError::storage)?;
        if claimed {
            Err(LedgerError::invalid_state(
                "transaction belongs to a transfer conversion; undo the whole conversion",
            ))
        } else {
            Ok(())
        }
    }
    async fn conversion_candidates(
        &mut self,
        user: UserId,
        id: JournalEntryId,
        query: &ConversionCandidatesQuery,
    ) -> Result<Vec<ConversionCandidate>, LedgerError> {
        let rows = sqlx::query(r#"
            WITH start AS (
                SELECT p.signed_amount,p.currency,j.occurred_at,p.account_id FROM ledger.journal_entries j
                JOIN ledger.postings p ON p.user_id=j.user_id AND p.journal_entry_id=j.id
                JOIN ledger.accounts a ON a.user_id=p.user_id AND a.id=p.account_id
                WHERE j.user_id=$1 AND j.id=$2 AND a.visibility='user_visible'
            )
            SELECT j.id,p.account_id,COALESCE(a.description,j.description) AS description,j.occurred_at,p.currency,ABS(p.signed_amount) AS amount
            FROM ledger.journal_entries j JOIN ledger.postings p ON p.user_id=j.user_id AND p.journal_entry_id=j.id
            JOIN ledger.accounts account ON account.user_id=p.user_id AND account.id=p.account_id
            LEFT JOIN ledger.transaction_annotations a ON a.user_id=j.user_id AND a.journal_entry_id=j.id
            CROSS JOIN start s
            WHERE j.user_id=$1 AND j.id<>$2 AND account.visibility='user_visible' AND account.lifecycle='active'
              AND p.account_id<>s.account_id AND p.signed_amount*s.signed_amount<0
              AND ($3::uuid IS NULL OR p.account_id=$3)
              AND j.source IN ('manual','import') AND j.purpose='ordinary'
              AND ($4::text IS NOT NULL OR ABS(EXTRACT(EPOCH FROM j.occurred_at-s.occurred_at))<=604800)
              AND ($4::text IS NULL OR COALESCE(a.description,j.description) ILIKE '%'||$4||'%')
              AND NOT EXISTS(SELECT 1 FROM ledger.journal_entries r WHERE r.user_id=j.user_id AND (r.reverses_transaction_id=j.id OR r.replaces_transaction_id=j.id))
              AND NOT EXISTS(SELECT 1 FROM ledger.transfer_conversion_journals c WHERE c.user_id=j.user_id AND c.journal_id=j.id AND c.role<>'restoration')
              AND NOT EXISTS(SELECT 1 FROM ledger.workflow_journal_claims w WHERE w.user_id=j.user_id AND w.journal_id=j.id)
              AND (SELECT COUNT(*) FROM ledger.postings pp JOIN ledger.accounts aa ON aa.user_id=pp.user_id AND aa.id=pp.account_id WHERE pp.user_id=j.user_id AND pp.journal_entry_id=j.id AND aa.visibility='user_visible')=1
              AND EXISTS(SELECT 1 FROM ledger.postings pp WHERE pp.user_id=j.user_id AND pp.journal_entry_id=j.id AND pp.account_nature IN ('income','expense'))
              AND NOT EXISTS(SELECT 1 FROM ledger.reclassification_details d WHERE d.user_id=j.user_id AND d.source_journal_entry_id=j.id AND NOT EXISTS(SELECT 1 FROM ledger.journal_entries r WHERE r.user_id=d.user_id AND r.reverses_transaction_id=d.journal_entry_id))
            ORDER BY (p.currency=s.currency AND ABS(p.signed_amount)=ABS(s.signed_amount)) DESC, ABS(EXTRACT(EPOCH FROM j.occurred_at-s.occurred_at)),j.id
            LIMIT 50 OFFSET $5
        "#).bind(user.into_uuid()).bind(id.into_uuid()).bind(query.other_account_id.map(|i| i.into_uuid())).bind(query.search.as_deref().filter(|s| !s.trim().is_empty())).bind(i64::from(query.offset.unwrap_or(0)))
        .fetch_all(&mut *self.transaction).await.map_err(LedgerError::storage)?;
        rows.into_iter()
            .map(|r| {
                Ok(ConversionCandidate {
                    journal_id: JournalEntryId::new(r.get("id")),
                    account_id: LedgerAccountId::new(r.get("account_id")),
                    description: r.get("description"),
                    occurred_at: r.get("occurred_at"),
                    money: ConversionMoney {
                        amount: r.get("amount"),
                        currency: CurrencyCode::new(r.get::<String, _>("currency"))
                            .map_err(|_| LedgerError::currency_mismatch())?,
                    },
                })
            })
            .collect()
    }
    async fn find_conversion(
        &mut self,
        user: UserId,
        id: uuid::Uuid,
    ) -> Result<TransferConversion, LedgerError> {
        let value: serde_json::Value = sqlx::query_scalar("SELECT document FROM ledger.transfer_conversions WHERE user_id=$1 AND id=$2 FOR UPDATE")
            .bind(user.into_uuid()).bind(id).fetch_optional(&mut *self.transaction).await.map_err(LedgerError::storage)?.ok_or_else(LedgerError::not_found)?;
        serde_json::from_value(value).map_err(|e| LedgerError::persistence(e.to_string()))
    }
    async fn save_conversion(
        &mut self,
        user: UserId,
        c: &TransferConversion,
    ) -> Result<(), LedgerError> {
        sqlx::query("INSERT INTO ledger.transfer_conversions(id,user_id,version,active,document) VALUES($1,$2,$3,$4,$5) ON CONFLICT(id,user_id) DO UPDATE SET version=EXCLUDED.version,active=EXCLUDED.active,document=EXCLUDED.document")
            .bind(c.id).bind(user.into_uuid()).bind(c.version).bind(c.active).bind(serde_json::to_value(c).map_err(|e| LedgerError::persistence(e.to_string()))?).execute(&mut *self.transaction).await.map_err(LedgerError::storage)?;
        for (original, restored) in &c.restorations {
            sqlx::query("INSERT INTO ledger.transfer_restorations(user_id,original_id,restored_id) VALUES($1,$2,$3) ON CONFLICT(user_id,original_id) DO NOTHING")
                .bind(user.into_uuid()).bind(original.into_uuid()).bind(restored.into_uuid()).execute(&mut *self.transaction).await.map_err(LedgerError::storage)?;
        }
        Ok(())
    }
    async fn claim_conversion_journal(
        &mut self,
        user: UserId,
        conversion: uuid::Uuid,
        journal: JournalEntryId,
        role: &str,
    ) -> Result<(), LedgerError> {
        sqlx::query("INSERT INTO ledger.transfer_conversion_journals(user_id,journal_id,conversion_id,role) VALUES($1,$2,$3,$4)")
            .bind(user.into_uuid()).bind(journal.into_uuid()).bind(conversion).bind(role).execute(&mut *self.transaction).await.map_err(LedgerError::storage)?;
        Ok(())
    }
    async fn restore_conversion_annotation(
        &mut self,
        user: UserId,
        source: JournalEntryId,
        restored: JournalEntryId,
    ) -> Result<(), LedgerError> {
        sqlx::query("INSERT INTO ledger.transaction_annotations(id,user_id,journal_entry_id,description,category_id,note,tags,budget_visibility,version,created_at,updated_at,assignment_origin,classification_decision_id,automation_state) SELECT $3,user_id,$3,description,category_id,note,tags,budget_visibility,1,created_at,updated_at,assignment_origin,classification_decision_id,automation_state FROM ledger.transaction_annotations WHERE user_id=$1 AND journal_entry_id=$2")
            .bind(user.into_uuid()).bind(source.into_uuid()).bind(restored.into_uuid()).execute(&mut *self.transaction).await.map_err(LedgerError::storage)?;
        Ok(())
    }
    async fn require_conversion_available(
        &mut self,
        user: UserId,
        id: JournalEntryId,
    ) -> Result<(), LedgerError> {
        let claimed:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM ledger.workflow_journal_claims WHERE user_id=$1 AND journal_id=$2)").bind(user.into_uuid()).bind(id.into_uuid()).fetch_one(&mut *self.transaction).await.map_err(LedgerError::storage)?;
        if claimed {
            return Err(LedgerError::invalid_state(
                "resolve the linked workflow before conversion",
            ));
        }
        Ok(())
    }
    async fn conversion_currency_scale(
        &mut self,
        _currency: &CurrencyCode,
    ) -> Result<u32, LedgerError> {
        // API resolves the currency minor unit; the storage bound is also checked here.
        Ok(8)
    }
}
