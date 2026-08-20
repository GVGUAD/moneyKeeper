use std::collections::BTreeMap;

use moneykeeper::contexts::banking::public::{
    Aes256CredentialCipher, CredentialBinding, CredentialCipher, FundingModel, MonobankAdapter,
    ProviderCredential, ProviderFailureClass, ResourceKind,
};
use moneykeeper::shared_kernel::{CurrencyCode, UserId};
use uuid::Uuid;

#[test]
fn credential_encryption_is_bound_to_tenant_connection_provider_and_generation() {
    let cipher = Aes256CredentialCipher::new("banking-key-1", [7_u8; 32]).unwrap();
    let user_id = UserId::new(Uuid::from_u128(1));
    let connection_id = Uuid::from_u128(2);
    let binding = CredentialBinding::new(user_id, connection_id, "monobank", 1, "active").unwrap();
    let secret = ProviderCredential::new("sanitized-x-token").unwrap();
    assert_eq!(format!("{secret:?}"), "ProviderCredential([REDACTED])");

    let encrypted = cipher.encrypt(&secret, &binding).unwrap();
    assert!(!format!("{encrypted:?}").contains("sanitized"));
    let decrypted = cipher.decrypt(&encrypted, &binding).unwrap();
    assert_eq!(decrypted.expose(), "sanitized-x-token");

    let wrong_user = CredentialBinding::new(
        UserId::new(Uuid::from_u128(3)),
        connection_id,
        "monobank",
        1,
        "active",
    )
    .unwrap();
    assert!(cipher.decrypt(&encrypted, &wrong_user).is_err());
    let wrong_generation =
        CredentialBinding::new(user_id, connection_id, "monobank", 2, "active").unwrap();
    assert!(cipher.decrypt(&encrypted, &wrong_generation).is_err());
}

#[test]
fn monobank_acl_discovers_cards_current_accounts_and_jars_without_securities_guessing() {
    let fixture = r#"{
      "clientId":"redacted-client",
      "name":"Example",
      "accounts":[
        {"id":"card-1","currencyCode":980,"cashbackType":"None","balance":120050,
         "creditLimit":0,"maskedPan":["4444******1111"],"type":"black","iban":"UA********42"},
        {"id":"credit-1","currencyCode":840,"cashbackType":"None","balance":-2200,
         "creditLimit":100000,"maskedPan":["5555******2222"],"type":"white","iban":"UA********43"},
        {"id":"mystery","currencyCode":980,"cashbackType":"None","balance":0,
         "creditLimit":0,"maskedPan":[],"type":"brokerage","iban":""}
      ],
      "jars":[
        {"id":"jar-1","sendId":"send","title":"Emergency","description":"",
         "currencyCode":980,"balance":50000,"goal":100000}
      ]
    }"#;
    let currencies = BTreeMap::from([
        (980_u16, (CurrencyCode::new("UAH").unwrap(), 2_u8)),
        (840_u16, (CurrencyCode::new("USD").unwrap(), 2_u8)),
    ]);
    let snapshot = MonobankAdapter::normalize_client_info(fixture, &currencies).unwrap();
    assert_eq!(snapshot.resources.len(), 4);
    assert_eq!(snapshot.resources[0].kind, ResourceKind::Card);
    assert_eq!(snapshot.resources[0].funding_model, FundingModel::OwnFunds);
    assert_eq!(
        snapshot.resources[1].funding_model,
        FundingModel::RevolvingCredit
    );
    assert_eq!(snapshot.resources[2].kind, ResourceKind::Unsupported);
    assert_eq!(snapshot.resources[3].kind, ResourceKind::Jar);
    assert!(
        !snapshot
            .resources
            .iter()
            .any(|resource| resource.kind == ResourceKind::SecurityPortfolio)
    );
    assert!(!format!("{snapshot:?}").contains("redacted-client"));
}

#[test]
fn provider_failures_are_retry_classified_without_response_bodies() {
    assert_eq!(
        MonobankAdapter::classify_status(429),
        ProviderFailureClass::RateLimited
    );
    assert_eq!(
        MonobankAdapter::classify_status(503),
        ProviderFailureClass::Transient
    );
    assert_eq!(
        MonobankAdapter::classify_status(401),
        ProviderFailureClass::NeedsReauth
    );
    assert_eq!(
        MonobankAdapter::classify_status(400),
        ProviderFailureClass::Terminal
    );
}
