use chrono::{TimeZone, Utc};
use moneykeeper::contexts::banking::public::{
    BalanceBasis, BalanceComparability, BalanceObservation, ConnectionState, ConnectionVersion,
    CredentialEnvelope, EventProcessingState, ExternalResource, ExternalResourceId, FundingModel,
    MappingDecision, ProviderConnection, ProviderConnectionId, ProviderEvent,
    ProviderEventIdentity, ProviderTransactionState, ResourceKind, SyncJob, SyncJobState,
};
use moneykeeper::contexts::ledger::public::{AccountKind, AccountNature, LedgerAccountId};
use moneykeeper::shared_kernel::{CurrencyCode, Money, UserId};
use rust_decimal_macros::dec;
use uuid::Uuid;

fn user() -> UserId {
    UserId::new(Uuid::from_u128(1))
}

fn currency() -> CurrencyCode {
    CurrencyCode::new("UAH").unwrap()
}

fn money(amount: rust_decimal::Decimal) -> Money {
    Money::new(amount, currency(), 2).unwrap()
}

fn at(second: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 20, 12, 0, second)
        .unwrap()
}

#[test]
fn connection_versions_credential_replacement_and_disconnect() {
    let mut connection = ProviderConnection::request(
        user(),
        "monobank",
        CredentialEnvelope::new("key-1", vec![1, 2, 3], vec![4, 5, 6]).unwrap(),
        at(0),
    )
    .unwrap();
    assert_eq!(connection.state(), ConnectionState::Pending);
    assert!(!format!("{connection:?}").contains("[1, 2, 3]"));

    connection
        .activate(ConnectionVersion::INITIAL, at(1))
        .unwrap();
    connection
        .request_credential_replacement(
            CredentialEnvelope::new("key-2", vec![7, 8], vec![9, 10]).unwrap(),
            connection.version(),
            at(2),
        )
        .unwrap();
    assert_eq!(
        connection.state(),
        ConnectionState::PendingCredentialValidation
    );
    assert!(connection.activate_candidate(connection.version(), at(3)).is_ok());
    assert_eq!(connection.credential_generation(), 2);
    assert!(connection
        .disconnect(ConnectionVersion::INITIAL, at(4))
        .is_err());
    connection
        .disconnect(connection.version(), at(4))
        .unwrap();
    assert_eq!(connection.state(), ConnectionState::Revoked);
    assert!(!connection.has_usable_credential());
}

#[test]
fn resources_are_stable_cash_like_mapping_roots() {
    let mut resource = ExternalResource::discover(
        user(),
        ProviderConnectionId::new(Uuid::from_u128(20)),
        "external-card-1",
        ResourceKind::Card,
        FundingModel::OwnFunds,
        currency(),
        "•••• 1234",
        at(0),
    )
    .unwrap();
    assert_eq!(
        resource.mapping_decision(AccountKind::DebitCard, AccountNature::Asset),
        MappingDecision::Allowed
    );
    assert_eq!(
        resource.mapping_decision(AccountKind::CreditCard, AccountNature::Liability),
        MappingDecision::IncompatibleAccount
    );

    let account = LedgerAccountId::new(Uuid::from_u128(10));
    resource.map(account, resource.version(), at(1)).unwrap();
    assert!(resource.map(account, resource.version(), at(2)).is_err());
    assert!(resource.change_currency(CurrencyCode::new("USD").unwrap()).is_err());
    resource
        .deactivate_mapping(resource.version(), "wrong account", at(3))
        .unwrap();
    resource.map(account, resource.version(), at(4)).unwrap();
    assert_eq!(resource.mapping_history().len(), 2);

    let portfolio = ExternalResource::discover(
        user(),
        ProviderConnectionId::new(Uuid::from_u128(20)),
        "future-securities",
        ResourceKind::SecurityPortfolio,
        FundingModel::Unknown,
        currency(),
        "portfolio",
        at(0),
    )
    .unwrap();
    assert_eq!(
        portfolio.mapping_decision(AccountKind::Current, AccountNature::Asset),
        MappingDecision::RouteToPortfolio
    );
}

#[test]
fn provider_event_revision_identity_and_transition_classification_are_explicit() {
    let identity = ProviderEventIdentity::new(
        ProviderConnectionId::new(Uuid::from_u128(1)),
        ExternalResourceId::new(Uuid::from_u128(2)),
        "statement-item",
        1,
    )
    .unwrap();
    let first = ProviderEvent::record(
        user(),
        identity.clone(),
        ProviderTransactionState::Pending,
        money(dec!(-10.00)),
        "coffee",
        [1; 32],
        at(0),
        at(1),
    )
    .unwrap();
    let settled = first
        .next_revision(
            ProviderEventIdentity::new(
                identity.connection_id(),
                identity.resource_id(),
                "statement-item",
                2,
            )
            .unwrap(),
            ProviderTransactionState::Settled,
            money(dec!(-10.00)),
            [2; 32],
            at(2),
            at(3),
        )
        .unwrap();
    assert!(settled.is_non_monetary_revision_of(&first));

    let corrected = settled
        .next_revision(
            ProviderEventIdentity::new(
                identity.connection_id(),
                identity.resource_id(),
                "statement-item",
                3,
            )
            .unwrap(),
            ProviderTransactionState::Settled,
            money(dec!(-12.00)),
            [3; 32],
            at(4),
            at(5),
        )
        .unwrap();
    assert!(corrected.is_monetary_revision_of(&settled));
    assert_eq!(first.processing_state(), EventProcessingState::Ready);
}

#[test]
fn sync_pages_advance_only_after_terminal_event_outcomes_and_are_fenced() {
    let mut job = SyncJob::request(
        user(),
        ProviderConnectionId::new(Uuid::from_u128(1)),
        at(0),
        at(20),
    )
    .unwrap();
    let lease = job.claim("worker-a", at(1), at(10)).unwrap();
    job.begin_page(&lease, "cursor-0", 2, at(2)).unwrap();
    assert!(job.complete_page(&lease, 1, 0, at(3)).is_err());
    job.complete_page(&lease, 1, 1, at(3)).unwrap();
    assert_eq!(job.state(), SyncJobState::Completed);
    assert_eq!(job.cursor(), Some("cursor-0"));

    let mut second = SyncJob::request(
        user(),
        ProviderConnectionId::new(Uuid::from_u128(2)),
        at(0),
        at(20),
    )
    .unwrap();
    let stale = second.claim("worker-a", at(1), at(2)).unwrap();
    let current = second.claim("worker-b", at(3), at(10)).unwrap();
    assert!(second.begin_page(&stale, "old", 0, at(4)).is_err());
    assert!(second.begin_page(&current, "current", 0, at(4)).is_ok());
}

#[test]
fn observations_retain_basis_and_non_comparable_reason() {
    let mut observation = BalanceObservation::record(
        user(),
        ExternalResourceId::new(Uuid::from_u128(1)),
        7,
        BalanceBasis::Available,
        money(dec!(100.00)),
        BalanceComparability::NotComparable("credit semantics unknown".into()),
        at(0),
        at(1),
    )
    .unwrap();
    assert_eq!(observation.source_sequence(), 7);
    assert!(observation.comparable_money().is_none());
    assert!(observation.mark_delivered(None).is_err());
}
