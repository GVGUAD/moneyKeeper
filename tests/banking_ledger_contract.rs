mod v2_test_support;

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
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
