mod v2_test_support;
use chrono::Utc;
use moneykeeper::{
    bootstrap::v2,
    contexts::sharing::public::*,
    shared_kernel::{CorrelationId, CurrencyCode, EventId, UserId},
};
use rust_decimal::Decimal;

fn metadata(user: UserId, sequence: u64) -> SharingEventMetadataV1 {
    SharingEventMetadataV1 {
        schema_version: 1,
        event_id: EventId::generate(),
        user_id: user,
        sequence,
        correlation_id: CorrelationId::generate(),
        causation_id: None,
        occurred_at: Utc::now(),
        recorded_at: Utc::now(),
    }
}

#[tokio::test]
async fn reporting_projects_deduplicates_cancels_and_rebuilds_bill_positions() {
    let (verified, pool) = v2_test_support::fresh_v2_runtime().await;
    let reporting = v2::supporting_contexts(&verified).reporting;
    let user = UserId::generate();
    let bill = BillSplitId::generate();
    let positioned = SharingEventV1 {
        metadata: metadata(user, 1),
        fact: SharingEventFactV1::BillPositionChanged {
            position: BillPositionV1 {
                bill_id: bill,
                revision: 1,
                currency: CurrencyCode::new("UAH").unwrap(),
                receivable: Decimal::new(500, 2),
                payable: Decimal::ZERO,
            },
        },
    };
    assert!(
        reporting
            .apply_sharing_event(positioned.clone())
            .await
            .unwrap()
            .applied
    );
    assert!(
        !reporting
            .apply_sharing_event(positioned.clone())
            .await
            .unwrap()
            .applied
    );
    let amount: Decimal = sqlx::query_scalar(
        "SELECT receivable FROM reporting.bill_positions WHERE user_id=$1 AND bill_id=$2",
    )
    .bind(user.into_uuid())
    .bind(bill.into_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(amount, Decimal::new(500, 2));
    let cancelled = SharingEventV1 {
        metadata: metadata(user, 2),
        fact: SharingEventFactV1::BillCancelled {
            bill_id: bill,
            revision: 1,
            bill_version: BillVersion(3),
            reason: "cancelled".into(),
            cancelled_at: Utc::now(),
        },
    };
    reporting
        .apply_sharing_event(cancelled.clone())
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM reporting.bill_positions WHERE user_id=$1 AND bill_id=$2",
    )
    .bind(user.into_uuid())
    .bind(bill.into_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 0);
    reporting
        .rebuild_sharing(vec![positioned, cancelled])
        .await
        .unwrap();
    let rebuilt: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM reporting.bill_positions WHERE user_id=$1 AND bill_id=$2",
    )
    .bind(user.into_uuid())
    .bind(bill.into_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(rebuilt, 0);
}
