mod v2_test_support;

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use chrono::Duration;
use moneykeeper::contexts::{banking::{self, public::*}, ledger::public::{
    AccountKind, AccountNature, OpenAccount,
}};
use moneykeeper::shared_kernel::{
    CorrelationId, CurrencyCode, IdempotencyKey, Money, UserId,
};
use rust_decimal::Decimal;
use uuid::Uuid;

struct FixtureProvider;

#[async_trait]
impl ProviderClient for FixtureProvider {
    async fn client_info(&self, _credential: &ProviderCredential) -> Result<String, ProviderFailure> {
        Ok(r#"{"accounts":[{"id":"card-1","currencyCode":980,"balance":10000,"creditLimit":0,"maskedPan":["4444******1111"],"type":"black","iban":""}],"jars":[{"id":"jar-1","title":"Reserve","currencyCode":980,"balance":5000}]}"#.to_owned())
    }
}

#[tokio::test]
async fn mapping_existing_account_validates_public_ledger_contract_and_tenant() {
    let (verified, pool) = v2_test_support::fresh_v2_runtime().await;
    let supporting = moneykeeper::bootstrap::v2::supporting_contexts(&verified);
    let ledger = supporting.ledger;
    let banking = banking::build_with_ledger(
        &verified,
        Arc::new(Aes256CredentialCipher::new("test-key", [4_u8; 32]).unwrap()),
        Arc::new(FixtureProvider),
        ledger.clone(),
        supporting.currencies,
        [1_u8;32],
    );
    let user_id = UserId::new(Uuid::new_v4());
    let connection = banking.connect_provider(ConnectProvider {
        user_id, provider: "monobank".to_owned(), credential: ProviderCredential::new("token").unwrap(),
        idempotency_key: IdempotencyKey::new("connect-mapping").unwrap(), correlation_id: CorrelationId::generate(), requested_at: Utc::now(),
    }).await.unwrap().connection;
    banking.validate_and_discover(user_id, connection.id).await.unwrap();
    let resource_id = ExternalResourceId::new(sqlx::query_scalar::<_,Uuid>("SELECT id FROM banking.external_resources WHERE user_id=$1 AND external_resource_id='card-1'").bind(user_id.into_uuid()).fetch_one(&pool).await.unwrap());

    let currency = CurrencyCode::new("UAH").unwrap();
    let account = ledger.open_account(OpenAccount {
        user_id, name:"Mapped card".to_owned(), currency:currency.clone(), kind:AccountKind::DebitCard, nature:AccountNature::Asset,
        opening_balance:Money::new(Decimal::ZERO,currency,2).unwrap(), idempotency_key:IdempotencyKey::new("open-mapped").unwrap(), correlation_id:CorrelationId::generate(), causation_id:None, occurred_at:Utc::now(),
    }).await.unwrap().account;
    let mapped = banking.bind_existing_resource(BindExistingResource {
        user_id, resource_id, ledger_account_id:account.id, expected_resource_version:1,
        idempotency_key:IdempotencyKey::new("bind-existing").unwrap(), correlation_id:CorrelationId::generate(), requested_at:Utc::now(),
    }).await.unwrap();
    assert_eq!(mapped.mapping.ledger_account_id, Some(account.id));

    let observation_time=Utc::now();
    let observation=banking.record_balance_observation(RecordBalanceObservation{user_id,connection_id:connection.id,resource_id,basis:BalanceBasis::Reported,provider_money:Money::new(rust_decimal_macros::dec!(100.00),CurrencyCode::new("UAH").unwrap(),2).unwrap(),sign_semantics:"provider_native".to_owned(),comparability:BalanceComparability::Comparable(Money::new(rust_decimal_macros::dec!(100.00),CurrencyCode::new("UAH").unwrap(),2).unwrap()),observed_at:observation_time,recorded_at:observation_time,correlation_id:CorrelationId::generate()}).await.unwrap();
    let delivered=moneykeeper::integration::process_managers::banking_observation::deliver_balance_observation(&banking,&ledger,user_id,observation.id).await.unwrap();
    assert_eq!(delivered.state,"delivered");
    assert!(delivered.reconciliation_case_id.is_some());
    assert!(ledger.list_journals(user_id,None,100).await.unwrap().is_empty());
    let non_comparable=banking.record_balance_observation(RecordBalanceObservation{user_id,connection_id:connection.id,resource_id,basis:BalanceBasis::CreditLimit,provider_money:Money::new(rust_decimal_macros::dec!(0.00),CurrencyCode::new("UAH").unwrap(),2).unwrap(),sign_semantics:"provider_native".to_owned(),comparability:BalanceComparability::NotComparable("not a scalar account balance".to_owned()),observed_at:observation_time,recorded_at:observation_time,correlation_id:CorrelationId::generate()}).await.unwrap();
    let skipped=moneykeeper::integration::process_managers::banking_observation::deliver_balance_observation(&banking,&ledger,user_id,non_comparable.id).await.unwrap();
    assert!(skipped.replayed);
    assert_eq!(ledger.list_reconciliations(user_id).await.unwrap().len(),1);

    let other_user = UserId::new(Uuid::new_v4());
    let rejected = banking.bind_existing_resource(BindExistingResource {
        user_id:other_user, resource_id, ledger_account_id:account.id, expected_resource_version:2,
        idempotency_key:IdempotencyKey::new("cross-user").unwrap(), correlation_id:CorrelationId::generate(), requested_at:Utc::now(),
    }).await;
    assert!(rejected.is_err());
}

#[tokio::test]
async fn create_and_map_is_retry_safe_and_opens_provider_observed_account() {
    let (verified, pool) = v2_test_support::fresh_v2_runtime().await;
    let supporting = moneykeeper::bootstrap::v2::supporting_contexts(&verified);
    let ledger = supporting.ledger;
    let banking = banking::build_with_ledger(
        &verified,
        Arc::new(Aes256CredentialCipher::new("test-key", [5_u8; 32]).unwrap()),
        Arc::new(FixtureProvider),
        ledger.clone(),
        supporting.currencies,
        [1_u8;32],
    );
    let user_id=UserId::new(Uuid::new_v4());
    let connection=banking.connect_provider(ConnectProvider { user_id,provider:"monobank".to_owned(),credential:ProviderCredential::new("token").unwrap(),idempotency_key:IdempotencyKey::new("connect-create-map").unwrap(),correlation_id:CorrelationId::generate(),requested_at:Utc::now() }).await.unwrap().connection;
    banking.validate_and_discover(user_id,connection.id).await.unwrap();
    let resource_id=ExternalResourceId::new(sqlx::query_scalar::<_,Uuid>("SELECT id FROM banking.external_resources WHERE user_id=$1 AND external_resource_id='jar-1'").bind(user_id.into_uuid()).fetch_one(&pool).await.unwrap());
    let make_command=||CreateAndMapResource { user_id,resource_id,account_name:"Reserve jar".to_owned(),expected_resource_version:1,idempotency_key:IdempotencyKey::new("create-map").unwrap(),correlation_id:CorrelationId::new(Uuid::from_u128(77)),requested_at:Utc::now() };
    let first=banking.create_and_map_resource(make_command()).await.unwrap();
    let replay=banking.create_and_map_resource(make_command()).await.unwrap();
    assert_eq!(first.mapping.ledger_account_id,replay.mapping.ledger_account_id);
    let account=ledger.get_account(user_id,first.mapping.ledger_account_id.unwrap()).await.unwrap();
    assert_eq!(account.authority,moneykeeper::contexts::ledger::public::AccountAuthority::ProviderObserved);
    assert_eq!(account.kind,AccountKind::Jar);
}

#[tokio::test]
async fn provider_revisions_post_once_and_corrections_and_reversals_remain_visible() {
    let (verified,pool)=v2_test_support::fresh_v2_runtime().await;
    let supporting=moneykeeper::bootstrap::v2::supporting_contexts(&verified);let ledger=supporting.ledger;
    let banking=banking::build_with_ledger(&verified,Arc::new(Aes256CredentialCipher::new("test-key",[8_u8;32]).unwrap()),Arc::new(FixtureProvider),ledger.clone(),supporting.currencies,[1_u8;32]);
    let user=UserId::new(Uuid::new_v4());let now=Utc::now();
    let connection=banking.connect_provider(ConnectProvider{user_id:user,provider:"monobank".to_owned(),credential:ProviderCredential::new("token").unwrap(),idempotency_key:IdempotencyKey::new("connect-import").unwrap(),correlation_id:CorrelationId::generate(),requested_at:now}).await.unwrap().connection;
    banking.validate_and_discover(user,connection.id).await.unwrap();
    let resource=ExternalResourceId::new(sqlx::query_scalar::<_,Uuid>("SELECT id FROM banking.external_resources WHERE user_id=$1 AND external_resource_id='card-1'").bind(user.into_uuid()).fetch_one(&pool).await.unwrap());
    let account=ledger.open_account(OpenAccount{user_id:user,name:"Imported card".to_owned(),currency:CurrencyCode::new("UAH").unwrap(),kind:AccountKind::DebitCard,nature:AccountNature::Asset,opening_balance:Money::new(Decimal::ZERO,CurrencyCode::new("UAH").unwrap(),2).unwrap(),idempotency_key:IdempotencyKey::new("open-import").unwrap(),correlation_id:CorrelationId::generate(),causation_id:None,occurred_at:now}).await.unwrap().account;
    banking.bind_existing_resource(BindExistingResource{user_id:user,resource_id:resource,ledger_account_id:account.id,expected_resource_version:1,idempotency_key:IdempotencyKey::new("bind-import").unwrap(),correlation_id:CorrelationId::generate(),requested_at:now}).await.unwrap();
    let add=|revision,state,amount,offset|IntakeProviderEvent{user_id:user,connection_id:connection.id,resource_id:resource,external_event_id:"event-stream".to_owned(),revision,state,operation_money:Money::new(amount,CurrencyCode::new("UAH").unwrap(),2).unwrap(),description:"provider purchase".to_owned(),effective_at:now+Duration::seconds(offset),recorded_at:now+Duration::seconds(offset),correlation_id:CorrelationId::generate()};
    let pending=banking.intake_provider_event(add(1,ProviderTransactionState::Pending,rust_decimal_macros::dec!(-10.00),1)).await.unwrap();
    moneykeeper::integration::process_managers::banking_import::import_provider_revision(&banking,&ledger,user,pending.provider_event_id).await.unwrap();
    moneykeeper::integration::process_managers::banking_import::import_provider_revision(&banking,&ledger,user,pending.provider_event_id).await.unwrap();
    assert_eq!(ledger.list_journals(user,None,100).await.unwrap().len(),1);
    let settled=banking.intake_provider_event(add(2,ProviderTransactionState::Settled,rust_decimal_macros::dec!(-10.00),2)).await.unwrap();
    let settled_outcome=moneykeeper::integration::process_managers::banking_import::import_provider_revision(&banking,&ledger,user,settled.provider_event_id).await.unwrap();
    assert_eq!(settled_outcome.state,"no_financial_change");
    assert_eq!(ledger.list_journals(user,None,100).await.unwrap().len(),1);
    let corrected=banking.intake_provider_event(add(3,ProviderTransactionState::Settled,rust_decimal_macros::dec!(-12.00),3)).await.unwrap();
    moneykeeper::integration::process_managers::banking_import::import_provider_revision(&banking,&ledger,user,corrected.provider_event_id).await.unwrap();
    assert_eq!(ledger.list_journals(user,None,100).await.unwrap().len(),3);
    let reversed=banking.intake_provider_event(add(4,ProviderTransactionState::Reversed,rust_decimal_macros::dec!(-12.00),4)).await.unwrap();
    moneykeeper::integration::process_managers::banking_import::import_provider_revision(&banking,&ledger,user,reversed.provider_event_id).await.unwrap();
    assert_eq!(ledger.list_journals(user,None,100).await.unwrap().len(),4);
}
