//! Delivers Banking-owned observations to Ledger-owned reconciliation.

use sha2::{Digest, Sha256};

use crate::{contexts::{banking::public::{BalanceObservationDeliveryOutcome,BalanceObservationId,BankingError,BankingFacade},ledger::public::{LedgerFacade,ObservationId,ObserveProviderBalance,ReconciliationStatus,SourceReference}},shared_kernel::{CorrelationId,IdempotencyKey}};

pub async fn deliver_balance_observation(banking:&BankingFacade,ledger:&LedgerFacade,user_id:crate::shared_kernel::UserId,observation_id:BalanceObservationId)->Result<BalanceObservationDeliveryOutcome,BankingError>{
    let Some(work)=banking.claim_balance_observation(user_id,observation_id).await?else{return Ok(BalanceObservationDeliveryOutcome{observation_id,state:"not_comparable_or_complete".to_owned(),reconciliation_case_id:None,active_case_id:None,replayed:true});};
    let comparable=work.observation.comparable_money.clone().ok_or(BankingError::InvalidState)?;
    let source=SourceReference::new("banking",work.observation.resource_id.to_string(),work.observation.id.to_string()).map_err(|_|BankingError::InvalidValue("invalid observation source"))?;
    let digest=Sha256::digest(format!("{}|{}",work.observation.resource_id,work.observation.id));let mut hex=String::new();for byte in digest{use std::fmt::Write as _;write!(&mut hex,"{byte:02x}").expect("String writes cannot fail");}
    let result=ledger.observe_provider_balance(ObserveProviderBalance{user_id,account_id:work.ledger_account_id,observation_id:ObservationId::new(observation_id.into_uuid()),source,provider_reported:comparable,available:None,observed_at:work.observation.observed_at,source_sequence:work.observation.source_sequence,idempotency_key:IdempotencyKey::new(format!("banking-observation-{hex}")).map_err(|_|BankingError::InvalidValue("invalid observation idempotency key"))?,correlation_id:CorrelationId::new(observation_id.into_uuid()),causation_id:None}).await.map_err(|_|BankingError::InvalidState)?;
    let ignored=result.case.status==ReconciliationStatus::IgnoredOlder;banking.complete_balance_observation(BalanceObservationDeliveryOutcome{observation_id,state:if ignored{"ignored_older".to_owned()}else{"delivered".to_owned()},reconciliation_case_id:(!ignored).then_some(result.case.id),active_case_id:ignored.then_some(result.case.id),replayed:false}).await
}
