//! Causal, idempotent provider-revision import through public context contracts.

use sha2::{Digest, Sha256};

use crate::{
    contexts::{
        banking::public::{
            BankingError, BankingFacade, ProviderEventId, ProviderImportOutcome,
            ProviderTransactionState as BankingTransactionState,
        },
        ledger::public::{
            ImportProviderTransaction, InternalCommandMetadata, LedgerFacade,
            ProviderTransactionState, ReverseProviderTransaction, SourceReference,
        },
    },
    shared_kernel::{CorrelationId, IdempotencyKey},
};

pub async fn import_provider_revision(
    banking: &BankingFacade,
    ledger: &LedgerFacade,
    user_id: crate::shared_kernel::UserId,
    event_id: ProviderEventId,
) -> Result<ProviderImportOutcome, BankingError> {
    let Some(work)=banking.claim_provider_import(user_id,event_id).await? else{return Ok(ProviderImportOutcome{provider_event_id:event_id,state:"waiting_or_complete".to_owned(),ledger_journal_entry_id:None,replayed:true});};
    let source=SourceReference::new("banking",format!("{}:{}",work.connection_id,work.resource_id),format!("{}:{}",work.external_event_id,work.revision)).map_err(|_|BankingError::InvalidValue("invalid provider source reference"))?;
    let correlation_id=CorrelationId::new(work.provider_event_id.into_uuid());
    let metadata=|operation:&str|->Result<InternalCommandMetadata,BankingError>{Ok(InternalCommandMetadata{user_id:work.user_id,source:source.clone(),correlation_id,causation_id:None,idempotency_key:key(operation,&work)?,occurred_at:work.effective_at})};
    let no_change=work.previous_money.as_ref()==Some(&work.operation_money)&&work.state==BankingTransactionState::Settled;
    let journal_id=if no_change{work.previous_journal_id}else if work.state==BankingTransactionState::Reversed{
        let previous=work.previous_journal_id.ok_or(BankingError::InvalidState)?;
        ledger.reverse_provider_transaction(ReverseProviderTransaction{metadata:metadata("reverse")?,imported_journal_entry_id:previous,reason:"provider reversal".to_owned()}).await.map_err(|_|BankingError::InvalidState)?.journal_entry_id
    }else{
        if work.previous_money.as_ref().is_some_and(|money|money!=&work.operation_money){
            let previous=work.previous_journal_id.ok_or(BankingError::InvalidState)?;
            ledger.reverse_provider_transaction(ReverseProviderTransaction{metadata:metadata("correct-reverse")?,imported_journal_entry_id:previous,reason:"provider monetary correction".to_owned()}).await.map_err(|_|BankingError::InvalidState)?;
        }
        ledger.import_provider_transaction(ImportProviderTransaction{metadata:metadata("post")?,user_account_id:work.ledger_account_id,amount:work.operation_money.clone(),state:ProviderTransactionState::Posted,description:work.description.clone()}).await.map_err(|_|BankingError::InvalidState)?.journal_entry_id
    };
    banking.complete_provider_import(ProviderImportOutcome{provider_event_id:event_id,state:if no_change{"no_financial_change".to_owned()}else{"posted".to_owned()},ledger_journal_entry_id:journal_id,replayed:false}).await
}

fn key(operation:&str,work:&crate::contexts::banking::public::ProviderImportWork)->Result<IdempotencyKey,BankingError>{
    let digest=Sha256::digest(format!("{operation}|{}|{}|{}|{}|{}",work.connection_id,work.resource_id,work.external_event_id,work.revision,work.provider_event_id));
    let mut encoded=String::with_capacity(64);for byte in digest{use std::fmt::Write as _;write!(&mut encoded,"{byte:02x}").expect("writing to String cannot fail");}
    IdempotencyKey::new(format!("banking-import-{operation}-{encoded}")).map_err(|_|BankingError::InvalidValue("invalid import idempotency key"))
}
