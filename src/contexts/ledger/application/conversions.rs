//! Atomic conversion, preview and restoration orchestration.
use super::{
    accounts::LedgerApplication, commit::commit_journal, ports::*, transfers::build_postings,
};
use crate::contexts::ledger::{conversion::principals, public::*};
use crate::shared_kernel::{Clock, CorrelationId, IdempotencyKey, Money, UserId};
use rust_decimal::Decimal;
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

impl<U: LedgerUnitOfWork, Q, P> LedgerApplication<U, Q, P> {
    pub async fn transfer_conversion(
        &self,
        user: UserId,
        action: ConversionAction,
    ) -> Result<ConversionResponse, LedgerError> {
        let mut tx = self.uow.begin().await?;
        tx.lock_conversion_scope(user).await?;
        let result = match action {
            ConversionAction::AttachmentCandidates { id, mut query } => {
                let c = tx.find_conversion(user, id).await?;
                if !c.active || c.sources.len() != 1 {
                    return Err(LedgerError::invalid_state(
                        "conversion has no missing original",
                    ));
                }
                let original = tx
                    .find_journal(user, c.sources[0].journal_id, false)
                    .await?
                    .ok_or_else(LedgerError::not_found)?;
                let movement = [&c.preview.outgoing, &c.preview.incoming]
                    .into_iter()
                    .find(|m| {
                        !original
                            .postings
                            .iter()
                            .any(|p| p.account_id() == m.account_id)
                    })
                    .ok_or_else(LedgerError::not_found)?;
                query.other_account_id = Some(movement.account_id);
                let candidates = tx
                    .conversion_candidates(user, c.sources[0].journal_id, &query)
                    .await?;
                let mut eligible = Vec::new();
                for candidate in candidates {
                    if candidate.money == movement.money
                        && tx
                            .find_journal(user, candidate.journal_id, false)
                            .await?
                            .is_some_and(|j| j.source == JournalSource::Import)
                    {
                        eligible.push(candidate);
                    }
                }
                ConversionResponse::Candidates(eligible)
            }

            ConversionAction::Notifications => {
                ConversionResponse::Notifications(tx.conversion_notifications(user).await?)
            }
            ConversionAction::Attach {
                id,
                journal_id,
                expected_version,
                key,
            } => {
                let hash = hash(&json!({"id":id,"journal":journal_id,"version":expected_version}))?;
                if let Some(c) =
                    receipt(&mut tx, user, "attach_conversion_import", &key, &hash).await?
                {
                    tx.rollback().await?;
                    return Ok(ConversionResponse::Conversion(c));
                }
                let mut c = tx.find_conversion(user, id).await?;
                if !c.active || c.version != expected_version || c.sources.len() != 1 {
                    return Err(LedgerError::version_conflict());
                }
                let (original, account, amount) = eligible(&mut tx, user, journal_id).await?;
                let existing = tx
                    .find_journal(user, c.sources[0].journal_id, true)
                    .await?
                    .ok_or_else(LedgerError::not_found)?;
                if original.source != JournalSource::Import
                    || existing
                        .postings
                        .iter()
                        .any(|p| p.account_id() == account.id())
                    || ![&c.preview.outgoing, &c.preview.incoming].iter().any(|m| {
                        m.account_id == account.id()
                            && m.signed_amount == amount
                            && m.money.currency == *account.currency()
                    })
                {
                    return Err(LedgerError::invalid_state(
                        "import must exactly match the created side",
                    ));
                }
                let source = tx.conversion_source(user, journal_id).await?;
                let reversed =
                    post_copy(&mut tx, self.clock.as_ref(), user, &original, &source, true).await?;
                tx.claim_conversion_journal(user, id, journal_id, "source")
                    .await?;
                tx.claim_conversion_journal(user, id, reversed, "reversal")
                    .await?;
                c.sources.push(source);
                c.generated_journal_ids.push(reversed);
                c.version += 1;
                c.lifecycle.push(ConversionLifecycle {
                    action: "posted_import_attached".into(),
                    at: self.clock.now(),
                    journal_ids: vec![journal_id, reversed],
                });
                save_conversion(&mut tx, self.clock.as_ref(), user, &c).await?;
                store_receipt(
                    &mut tx,
                    user,
                    "attach_conversion_import",
                    &key,
                    &hash,
                    &c,
                    self.clock.as_ref(),
                )
                .await?;
                ConversionResponse::Conversion(c)
            }

            ConversionAction::Reviews { id } => {
                ConversionResponse::Reviews(tx.conversion_reviews(user, id).await?)
            }
            ConversionAction::ResolveReview {
                id,
                conversion_id,
                expected_version,
                key,
            } => {
                let hash = hash(
                    &json!({"review":id,"conversion":conversion_id,"version":expected_version}),
                )?;
                if let Some(r) = tx
                    .find_receipt(user, "resolve_conversion_review", &key, true)
                    .await?
                {
                    if r.request_hash != hash {
                        return Err(LedgerError::idempotency_conflict());
                    }
                    let result = serde_json::from_value(r.result)
                        .map_err(|e| LedgerError::persistence(e.to_string()))?;
                    tx.rollback().await?;
                    return Ok(result);
                }
                let mut review = tx
                    .conversion_reviews(user, Some(id))
                    .await?
                    .pop()
                    .ok_or_else(LedgerError::not_found)?;
                if review.version != expected_version || review.state != "pending_review" {
                    return Err(LedgerError::version_conflict());
                }
                if let Some(cid) = conversion_id {
                    if !review.candidates.contains(&cid) {
                        return Err(LedgerError::invalid_state("select a suggested conversion"));
                    }
                    let mut c = tx.find_conversion(user, cid).await?;
                    if !c.active || c.sources.len() != 1 {
                        return Err(LedgerError::version_conflict());
                    }
                    let journal =
                        post_evidence(&mut tx, self.clock.as_ref(), user, &review).await?;
                    let source = tx.conversion_source(user, journal).await?;
                    let original = tx
                        .find_journal(user, journal, true)
                        .await?
                        .ok_or_else(LedgerError::not_found)?;
                    let reversal =
                        post_copy(&mut tx, self.clock.as_ref(), user, &original, &source, true)
                            .await?;
                    tx.claim_conversion_journal(user, cid, journal, "source")
                        .await?;
                    tx.claim_conversion_journal(user, cid, reversal, "reversal")
                        .await?;
                    c.sources.push(source);
                    c.generated_journal_ids.push(reversal);
                    c.version += 1;
                    c.lifecycle.push(ConversionLifecycle {
                        action: "bank_import_attached".into(),
                        at: self.clock.now(),
                        journal_ids: vec![journal, reversal],
                    });
                    save_conversion(&mut tx, self.clock.as_ref(), user, &c).await?;
                    review.state = "confirmed".into();
                    review.journal_id = Some(journal);
                    review.conversion_id = Some(cid);
                } else {
                    review.journal_id =
                        Some(post_evidence(&mut tx, self.clock.as_ref(), user, &review).await?);
                    review.state = "dismissed".into();
                }
                review.version += 1;
                tx.save_conversion_review(user, &review).await?;
                let result = ConversionResponse::Reviews(vec![review]);
                tx.insert_receipt(
                    user,
                    "resolve_conversion_review",
                    &key,
                    &hash,
                    &serde_json::to_value(&result)
                        .map_err(|e| LedgerError::persistence(e.to_string()))?,
                    self.clock.now(),
                )
                .await?;
                result
            }
            ConversionAction::ResolveProvider {
                previous,
                stream,
                item,
                changed,
            } => {
                let mut journal = previous;
                for mut review in tx.conversion_reviews(user, None).await? {
                    if review.stream == stream
                        && review.item.rsplit_once(':').map(|p| p.0)
                            == item.rsplit_once(':').map(|p| p.0)
                    {
                        journal = journal.or(review.journal_id);
                        if changed && review.state == "pending_review" {
                            review.state = "cancelled".into();
                            review.version += 1;
                            tx.save_conversion_review(user, &review).await?;
                        }
                    }
                }
                if let Some(j) = journal {
                    let j = tx.resolve_conversion_reference(user, j).await?;
                    if changed && let Some(cid) = tx.managed_conversion(user, j).await? {
                        let mut c = tx.find_conversion(user, cid).await?;
                        undo_conversion(&mut tx, self.clock.as_ref(), user, &mut c).await?;
                        c.lifecycle.push(ConversionLifecycle {
                            action: "bank_revision_automatically_undid_conversion".into(),
                            at: self.clock.now(),
                            journal_ids: vec![],
                        });
                        c.version += 1;
                        save_conversion(&mut tx, self.clock.as_ref(), user, &c).await?;
                    }
                    journal = Some(tx.resolve_conversion_reference(user, j).await?);
                }
                ConversionResponse::Reference {
                    journal_id: journal,
                }
            }

            ConversionAction::Candidates { journal_id, query } => {
                eligible(&mut tx, user, journal_id).await?;
                ConversionResponse::Candidates(
                    tx.conversion_candidates(user, journal_id, &query).await?,
                )
            }
            ConversionAction::Preview { journal_id, input } => {
                ConversionResponse::Preview(preview(&mut tx, user, journal_id, &input).await?.0)
            }
            ConversionAction::Get { id } => {
                ConversionResponse::Conversion(tx.find_conversion(user, id).await?)
            }
            ConversionAction::Edit {
                id,
                expected_version,
                title,
                note,
            } => {
                metadata(&title, note.as_deref())?;
                let mut c = tx.find_conversion(user, id).await?;
                if c.version != expected_version {
                    return Err(LedgerError::version_conflict());
                }
                c.title = title.trim().to_owned();
                c.note = note;
                c.version += 1;
                c.lifecycle.push(ConversionLifecycle {
                    action: "metadata_edited".into(),
                    at: self.clock.now(),
                    journal_ids: vec![],
                });
                save_conversion(&mut tx, self.clock.as_ref(), user, &c).await?;
                ConversionResponse::Conversion(c)
            }
            ConversionAction::Convert {
                journal_id,
                input,
                version_token,
                key,
            } => {
                let hash =
                    hash(&json!({"journal":journal_id,"input":input,"token":version_token}))?;
                if let Some(result) =
                    receipt(&mut tx, user, "convert_to_transfer", &key, &hash).await?
                {
                    tx.rollback().await?;
                    return Ok(ConversionResponse::Conversion(result));
                }
                let (p, sources, accounts) = preview(&mut tx, user, journal_id, &input)
                    .await
                    .map_err(|e| {
                        if e.is_persistence() || e.is_not_found() {
                            e
                        } else {
                            LedgerError::version_conflict()
                        }
                    })?;
                if p.version_token != version_token {
                    return Err(LedgerError::version_conflict());
                }
                let id = Uuid::new_v4();
                let mut generated = Vec::new();
                for source in &sources {
                    let original = tx
                        .find_journal(user, source.journal_id, true)
                        .await?
                        .ok_or_else(LedgerError::not_found)?;
                    let reversed =
                        post_copy(&mut tx, self.clock.as_ref(), user, &original, source, true)
                            .await?;
                    tx.claim_conversion_journal(user, id, source.journal_id, "source")
                        .await?;
                    tx.claim_conversion_journal(user, id, reversed, "reversal")
                        .await?;
                    generated.push(reversed);
                }
                let money = |m: &ConversionMoney| {
                    Money::new(m.amount, m.currency.clone(), 8)
                        .map_err(|e| LedgerError::invalid_money(e.to_string()))
                };
                let command = TransferFunds {
                    user_id: user,
                    source_account_id: p.outgoing.account_id,
                    target_account_id: p.incoming.account_id,
                    source_amount: money(&p.source_principal)?,
                    target_amount: money(&p.target_principal)?,
                    fee: p
                        .fee
                        .as_ref()
                        .map(|f| money(f).map(|amount| TransferFee { amount }))
                        .transpose()?,
                    implied_rate: p
                        .source_per_target_rate
                        .as_ref()
                        .map(|s| s.parse())
                        .transpose()
                        .map_err(|_| LedgerError::invalid_money("invalid rate"))?,
                    description: input.title.trim().into(),
                    occurred_at: p.occurred_at,
                    idempotency_key: unique_key()?,
                    correlation_id: CorrelationId::new(id),
                    causation_id: None,
                };
                let outgoing = accounts
                    .iter()
                    .find(|a| a.id() == p.outgoing.account_id)
                    .ok_or_else(LedgerError::not_found)?;
                let incoming = accounts
                    .iter()
                    .find(|a| a.id() == p.incoming.account_id)
                    .ok_or_else(LedgerError::not_found)?;
                let postings =
                    build_postings(&mut tx, self.clock.as_ref(), &command, outgoing, incoming)
                        .await?;
                let mut journal = JournalEntry::post(
                    JournalEntryId::generate(),
                    user,
                    &command.description,
                    PostingPurpose::Ordinary,
                    JournalSource::Manual,
                    Actor::User(user),
                    p.occurred_at,
                    self.clock.now(),
                    command.correlation_id,
                    None,
                    command.idempotency_key,
                    JournalRelations::none(),
                    postings,
                )?;
                if let Some(rate) = command.implied_rate {
                    journal = journal.with_fx_rate(rate)?;
                }
                commit_journal(
                    &mut tx,
                    "convert_to_transfer",
                    &journal,
                    None,
                    "ledger.journal-posted.v1",
                )
                .await?;
                generated.push(journal.id());
                tx.claim_conversion_journal(user, id, journal.id(), "transfer")
                    .await?;
                let c = TransferConversion {
                    id,
                    version: 1,
                    active: true,
                    title: command.description,
                    note: input.note,
                    preview: p,
                    sources,
                    transfer_journal_id: journal.id(),
                    generated_journal_ids: generated.clone(),
                    restorations: vec![],
                    lifecycle: vec![ConversionLifecycle {
                        action: "converted".into(),
                        at: self.clock.now(),
                        journal_ids: generated,
                    }],
                };
                save_conversion(&mut tx, self.clock.as_ref(), user, &c).await?;
                store_receipt(
                    &mut tx,
                    user,
                    "convert_to_transfer",
                    &key,
                    &hash,
                    &c,
                    self.clock.as_ref(),
                )
                .await?;
                ConversionResponse::Conversion(c)
            }
            ConversionAction::Undo {
                id,
                expected_version,
                key,
            } => {
                let hash = hash(&json!({"id":id,"version":expected_version}))?;
                if let Some(c) =
                    receipt(&mut tx, user, "undo_transfer_conversion", &key, &hash).await?
                {
                    tx.rollback().await?;
                    return Ok(ConversionResponse::Conversion(c));
                }
                let mut c = tx.find_conversion(user, id).await?;
                if c.version != expected_version || !c.active {
                    return Err(LedgerError::version_conflict());
                }
                undo_conversion(&mut tx, self.clock.as_ref(), user, &mut c).await?;
                store_receipt(
                    &mut tx,
                    user,
                    "undo_transfer_conversion",
                    &key,
                    &hash,
                    &c,
                    self.clock.as_ref(),
                )
                .await?;
                ConversionResponse::Conversion(c)
            }
        };
        tx.commit().await?;
        Ok(result)
    }
}
fn metadata(title: &str, note: Option<&str>) -> Result<(), LedgerError> {
    if title.trim().is_empty()
        || title.chars().count() > 500
        || note.is_some_and(|n| n.chars().count() > 2000)
    {
        return Err(LedgerError::invalid_annotation("title or note is invalid"));
    }
    Ok(())
}
async fn eligible<
    T: JournalStore + ConversionStore + LedgerAccountStore + ReclassificationStore,
>(
    tx: &mut T,
    user: UserId,
    id: JournalEntryId,
) -> Result<(JournalSnapshot, LedgerAccount, Decimal), LedgerError> {
    tx.require_unclaimed(user, id).await?;
    tx.require_conversion_available(user, id).await?;
    let j = tx
        .find_journal(user, id, true)
        .await?
        .ok_or_else(LedgerError::not_found)?;
    if j.reversed
        || j.replaced
        || j.purpose != PostingPurpose::Ordinary
        || !matches!(j.source, JournalSource::Manual | JournalSource::Import)
        || j.postings.len() != 2
    {
        return Err(LedgerError::invalid_state(
            "select a live ordinary income or expense",
        ));
    }
    let accounts = tx
        .lock_accounts(
            user,
            &j.postings
                .iter()
                .map(Posting::account_id)
                .collect::<Vec<_>>(),
        )
        .await?;
    let visible = accounts
        .iter()
        .filter(|a| a.visibility() == AccountVisibility::UserVisible)
        .collect::<Vec<_>>();
    if visible.len() != 1
        || !j.postings.iter().any(|p| {
            matches!(
                p.account_nature(),
                AccountNature::Income | AccountNature::Expense
            )
        })
    {
        return Err(LedgerError::invalid_state(
            "transaction must have one user account movement",
        ));
    }
    let account = visible[0].clone();
    account.require_posting_allowed(PostingPurpose::Ordinary)?;
    for nature in ["expense", "income"] {
        if tx.active_reclassified_amount(user, id, nature).await? > Decimal::ZERO {
            return Err(LedgerError::invalid_state(
                "resolve financial allocations first",
            ));
        }
    }
    let amount = j
        .postings
        .iter()
        .find(|p| p.account_id() == account.id())
        .ok_or_else(LedgerError::not_found)?
        .signed_amount();
    Ok((j, account, amount))
}
async fn preview<
    T: JournalStore + ConversionStore + LedgerAccountStore + ReclassificationStore + ProjectionStore,
>(
    tx: &mut T,
    user: UserId,
    id: JournalEntryId,
    input: &ConversionInput,
) -> Result<(ConversionPreview, Vec<ConversionSource>, Vec<LedgerAccount>), LedgerError> {
    metadata(&input.title, input.note.as_deref())?;
    if input.other_journal_id.is_some() == input.missing_side.is_some() {
        return Err(LedgerError::invalid_state(
            "choose one existing transaction or missing side",
        ));
    }
    let mut journal_ids = vec![id];
    journal_ids.extend(input.other_journal_id);
    journal_ids.sort();
    journal_ids.dedup();
    let mut account_ids = vec![input.other_account_id];
    for journal in journal_ids {
        let snapshot = tx
            .find_journal(user, journal, true)
            .await?
            .ok_or_else(LedgerError::not_found)?;
        account_ids.extend(snapshot.postings.iter().map(Posting::account_id));
    }
    account_ids.sort();
    account_ids.dedup();
    tx.lock_accounts(user, &account_ids).await?;
    let (_, start, start_amount) = eligible(tx, user, id).await?;
    if start.id() == input.other_account_id {
        return Err(LedgerError::invalid_state("choose another account"));
    }
    let mut sources = vec![tx.conversion_source(user, id).await?];
    let (other, other_amount) = if let Some(other_id) = input.other_journal_id {
        let (_, a, amount) = eligible(tx, user, other_id).await?;
        if a.id() != input.other_account_id
            || amount.is_sign_negative() == start_amount.is_sign_negative()
        {
            return Err(LedgerError::invalid_state(
                "counterpart must be the opposite movement in the selected account",
            ));
        }
        sources.push(tx.conversion_source(user, other_id).await?);
        (a, amount)
    } else {
        let a = tx
            .find_account(user, input.other_account_id, true)
            .await?
            .ok_or_else(LedgerError::not_found)?;
        if a.visibility() != AccountVisibility::UserVisible {
            return Err(LedgerError::not_found());
        }
        a.require_posting_allowed(PostingPurpose::Ordinary)?;
        let m = input
            .missing_side
            .as_ref()
            .ok_or_else(LedgerError::invalid_account_kind)?;
        if m.currency != *a.currency() || m.amount <= Decimal::ZERO {
            return Err(LedgerError::invalid_money(
                "invalid missing-side amount or currency",
            ));
        }
        (
            a,
            if start_amount < Decimal::ZERO {
                m.amount
            } else {
                -m.amount
            },
        )
    };
    for m in input.missing_side.iter().chain(input.fee.iter()) {
        Money::new(
            m.amount,
            m.currency.clone(),
            tx.conversion_currency_scale(&m.currency).await?,
        )
        .map_err(|e| LedgerError::invalid_money(e.to_string()))?;
    }
    let (outgoing, incoming, out_amount, in_amount) = if start_amount < Decimal::ZERO {
        (&start, &other, start_amount, other_amount)
    } else {
        (&other, &start, other_amount, start_amount)
    };
    let movement = |a: &LedgerAccount, amount: Decimal, version: i64| ConversionMovement {
        account_id: a.id(),
        account_name: a.name().into(),
        money: ConversionMoney {
            amount: amount.abs(),
            currency: a.currency().clone(),
        },
        signed_amount: amount,
        balance_change: if a.id() == start.id() || input.other_journal_id.is_some() {
            Decimal::ZERO
        } else {
            amount * Decimal::from(a.normal_sign())
        },
        balance_version: version,
    };
    let out_version = tx
        .signed_balance(user, outgoing.id(), true)
        .await?
        .ok_or_else(LedgerError::not_found)?
        .1;
    let in_version = tx
        .signed_balance(user, incoming.id(), true)
        .await?
        .ok_or_else(LedgerError::not_found)?
        .1;
    let out = movement(outgoing, out_amount, out_version);
    let incoming = movement(incoming, in_amount, in_version);
    let (source_principal, target_principal) = principals(
        &out.money,
        &incoming.money,
        input.fee.as_ref(),
        input.confirm_fee,
    )?;
    let rate = if source_principal.currency == target_principal.currency {
        None
    } else {
        Some(
            source_principal
                .amount
                .checked_div(target_principal.amount)
                .ok_or_else(LedgerError::unbalanced_journal)?
                .to_string(),
        )
    };
    let date = input.occurred_at.unwrap_or_else(|| {
        if start_amount < Decimal::ZERO || sources.len() == 1 {
            sources[0].occurred_at
        } else {
            sources[1].occurred_at
        }
    });
    let removed = if sources.len() == 2 {
        vec![out.money.clone(), incoming.money.clone()]
    } else {
        vec![ConversionMoney {
            amount: start_amount.abs(),
            currency: start.currency().clone(),
        }]
    };
    let mut p = ConversionPreview {
        version_token: String::new(),
        outgoing: out,
        incoming,
        source_principal,
        target_principal,
        fee: input.fee.clone(),
        source_per_target_rate: rate,
        occurred_at: date,
        removed_from_totals: removed,
        eligibility_issues: vec![],
    };
    p.version_token=hash(&json!({"user":user,"input":input,"preview":p,"sources":sources,"accounts":[start.version(),other.version()]}))?.iter().map(|b|format!("{b:02x}")).collect();
    Ok((p, sources, vec![start, other]))
}
async fn post_copy<
    T: JournalStore + AnnotationStore + ProjectionStore + AuditStore + LedgerOutboxStore,
>(
    tx: &mut T,
    clock: &dyn Clock,
    user: UserId,
    original: &JournalSnapshot,
    source: &ConversionSource,
    reverse: bool,
) -> Result<JournalEntryId, LedgerError> {
    // Restoration deliberately uses ordinary purpose and no reversal relation, even on archived accounts.
    let postings = original
        .postings
        .iter()
        .map(|p| {
            Posting::rehydrate(
                PostingId::generate(),
                p.position(),
                p.account_id(),
                user,
                p.currency().clone(),
                p.account_nature(),
                if reverse {
                    -p.signed_amount()
                } else {
                    p.signed_amount()
                },
            )
        })
        .collect();
    let j = JournalEntry::post(
        JournalEntryId::generate(),
        user,
        &source.description,
        if reverse {
            PostingPurpose::Reversal
        } else {
            PostingPurpose::Ordinary
        },
        if reverse {
            JournalSource::Correction
        } else {
            original.source
        },
        Actor::User(user),
        source.occurred_at,
        clock.now(),
        CorrelationId::new(Uuid::new_v4()),
        None,
        unique_key()?,
        if reverse {
            JournalRelations::reversal_of(original.id)
        } else {
            JournalRelations::none()
        },
        postings,
    )?;
    commit_journal(
        tx,
        if reverse {
            "conversion_reversal"
        } else {
            "conversion_restoration"
        },
        &j,
        None,
        if reverse {
            "ledger.journal-reversed.v1"
        } else {
            "ledger.journal-posted.v1"
        },
    )
    .await?;
    Ok(j.id())
}
fn unique_key() -> Result<IdempotencyKey, LedgerError> {
    IdempotencyKey::new(format!("conversion-{}", Uuid::new_v4()))
        .map_err(|e| LedgerError::persistence(e.to_string()))
}
fn hash(v: &serde_json::Value) -> Result<[u8; 32], LedgerError> {
    Ok(
        Sha256::digest(serde_json::to_vec(v).map_err(|e| LedgerError::persistence(e.to_string()))?)
            .into(),
    )
}
async fn receipt<T: CommandReceiptStore>(
    tx: &mut T,
    user: UserId,
    name: &str,
    key: &IdempotencyKey,
    hash: &[u8; 32],
) -> Result<Option<TransferConversion>, LedgerError> {
    match tx.find_receipt(user, name, key, true).await? {
        None => Ok(None),
        Some(r) => {
            if r.request_hash != *hash {
                return Err(LedgerError::idempotency_conflict());
            }
            serde_json::from_value(r.result)
                .map(Some)
                .map_err(|e| LedgerError::persistence(e.to_string()))
        }
    }
}
async fn store_receipt<T: CommandReceiptStore>(
    tx: &mut T,
    user: UserId,
    name: &str,
    key: &IdempotencyKey,
    hash: &[u8; 32],
    c: &TransferConversion,
    clock: &dyn Clock,
) -> Result<(), LedgerError> {
    tx.insert_receipt(
        user,
        name,
        key,
        hash,
        &serde_json::to_value(c).map_err(|e| LedgerError::persistence(e.to_string()))?,
        clock.now(),
    )
    .await
}

trait ConversionTransaction:
    ConversionStore
    + JournalStore
    + AnnotationStore
    + ProjectionStore
    + AuditStore
    + LedgerOutboxStore
    + LedgerAccountStore
    + ReclassificationStore
    + CommandReceiptStore
{
}
impl<
    T: ConversionStore
        + JournalStore
        + AnnotationStore
        + ProjectionStore
        + AuditStore
        + LedgerOutboxStore
        + LedgerAccountStore
        + ReclassificationStore
        + CommandReceiptStore,
> ConversionTransaction for T
{
}
async fn undo_conversion<T: ConversionTransaction>(
    tx: &mut T,
    clock: &dyn Clock,
    user: UserId,
    c: &mut TransferConversion,
) -> Result<(), LedgerError> {
    let id = c.id;
    let original = tx
        .find_journal(user, c.transfer_journal_id, true)
        .await?
        .ok_or_else(LedgerError::not_found)?;
    let source = tx.conversion_source(user, c.transfer_journal_id).await?;
    let reversal = post_copy(tx, clock, user, &original, &source, true).await?;
    tx.claim_conversion_journal(user, id, reversal, "reversal")
        .await?;
    let mut ids = vec![reversal];
    for source in &c.sources {
        let original = tx
            .find_journal(user, source.journal_id, true)
            .await?
            .ok_or_else(LedgerError::not_found)?;
        let restored = post_copy(tx, clock, user, &original, source, false).await?;
        tx.restore_conversion_annotation(user, source.journal_id, restored)
            .await?;
        tx.claim_conversion_journal(user, id, restored, "restoration")
            .await?;
        c.restorations.push((source.journal_id, restored));
        ids.push(restored);
    }
    c.active = false;
    c.version += 1;
    c.generated_journal_ids.extend(ids.iter().copied());
    c.lifecycle.push(ConversionLifecycle {
        action: "undone".into(),
        at: clock.now(),
        journal_ids: ids,
    });
    save_conversion(tx, clock, user, c).await?;

    for mut review in tx.conversion_reviews(user, None).await? {
        if review.state == "pending_review" && review.candidates.contains(&c.id) {
            review.journal_id = Some(post_evidence(tx, clock, user, &review).await?);
            review.state = "dismissed".into();
            review.version += 1;
            tx.save_conversion_review(user, &review).await?;
        }
    }
    Ok(())
}
async fn post_evidence<T: ConversionTransaction>(
    tx: &mut T,
    clock: &dyn Clock,
    user: UserId,
    r: &ConversionImportReview,
) -> Result<JournalEntryId, LedgerError> {
    let a = tx
        .find_account(user, r.account_id, true)
        .await?
        .ok_or_else(LedgerError::not_found)?;
    let role = if r.money.amount < Decimal::ZERO {
        SystemAccountRole::UncategorizedExpense
    } else {
        SystemAccountRole::UncategorizedIncome
    };
    let system = match tx
        .find_system_account(user, &r.money.currency, role, None)
        .await?
    {
        Some(a) => a,
        None => {
            let a = LedgerAccount::open_system(
                LedgerAccountId::generate(),
                user,
                r.money.currency.clone(),
                role,
                clock,
            );
            tx.insert_account(&a).await?;
            a
        }
    };
    let postings = vec![
        Posting::rehydrate(
            PostingId::generate(),
            1,
            a.id(),
            user,
            r.money.currency.clone(),
            a.nature(),
            r.money.amount,
        ),
        Posting::for_account(
            PostingId::generate(),
            &system,
            -r.money.amount,
            PostingPurpose::Ordinary,
        )?,
    ];
    let j = JournalEntry::post(
        JournalEntryId::generate(),
        user,
        &r.description,
        PostingPurpose::Ordinary,
        JournalSource::Import,
        Actor::External {
            source_kind: "banking".into(),
            source_reference: r.item.clone(),
        },
        r.occurred_at,
        clock.now(),
        CorrelationId::new(r.id),
        None,
        unique_key()?,
        JournalRelations::none(),
        postings,
    )?;
    let annotation = TransactionAnnotation::new(
        AnnotationId::new(j.id().into_uuid()),
        j.id(),
        user,
        &r.description,
        None,
        None,
        NormalizedTags::empty(),
        BudgetVisibility::Included,
        clock.now(),
    )?;
    commit_journal(
        tx,
        "conversion_import_evidence",
        &j,
        Some(&annotation),
        "ledger.journal-posted.v1",
    )
    .await?;
    Ok(j.id())
}

async fn save_conversion<T: ConversionStore + AuditStore + LedgerOutboxStore>(
    tx: &mut T,
    clock: &dyn Clock,
    user: UserId,
    c: &TransferConversion,
) -> Result<(), LedgerError> {
    tx.save_conversion(user, c).await?;
    let event_id = crate::shared_kernel::EventId::generate();
    let payload = json!({"conversion_id":c.id,"version":c.version,"active":c.active,"action":c.lifecycle.last().map(|e|&e.action),"source_journal_ids":c.sources.iter().map(|s|s.journal_id).collect::<Vec<_>>(),"transfer_journal_id":c.transfer_journal_id});
    tx.append_audit(&AuditRecord {
        event_id,
        user_id: user,
        aggregate_kind: "transfer_conversion",
        aggregate_id: c.id,
        event_type: "ledger.transfer-conversion-changed.v1",
        actor_kind: "user",
        actor_reference: Some(user.to_string()),
        correlation_id: c.id,
        payload: payload.clone(),
        occurred_at: clock.now(),
        recorded_at: clock.now(),
    })
    .await?;
    tx.append_outbox(&super::accounts::integration_event(
        event_id,
        user,
        c.id.to_string(),
        c.version as u64,
        "ledger.transfer-conversion-changed.v1",
        clock.now(),
        CorrelationId::new(c.id),
        None,
        payload,
    )?)
    .await
}
