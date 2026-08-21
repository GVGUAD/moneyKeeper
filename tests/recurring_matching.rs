use moneykeeper::{
    contexts::recurring::domain::{MatchId, RecurringError},
    integration::process_managers::recurring_match::{CategorizationState, RecurringMatchProcess},
};
#[path = "v2_test_support.rs"]
mod v2_test_support;
#[test]
fn unmatch_is_fenced_until_categorization_is_resolved() {
    let mut process = RecurringMatchProcess::start(MatchId::generate(), None, 0);
    assert_eq!(
        process.request_unmatch(0),
        Err(RecurringError::CategorizationPending)
    );
    process.annotation_posted(1).unwrap();
    process.request_unmatch(1).unwrap();
    assert_eq!(process.state, CategorizationState::Compensating);
    process.compensation_posted().unwrap();
    assert_eq!(process.state, CategorizationState::Compensated);
}
#[test]
fn newer_user_annotation_is_never_overwritten() {
    let mut process = RecurringMatchProcess::start(MatchId::generate(), None, 0);
    process.annotation_posted(1).unwrap();
    process.request_unmatch(2).unwrap();
    assert_eq!(
        process.state,
        CategorizationState::CompensationSkippedNewerAnnotation
    );
}

#[tokio::test]
async fn mail_evidence_is_consumed_once_with_version_zero_matching() {
    use chrono::Utc;
    use moneykeeper::{
        contexts::mail::public::{
            ReceiptEvidenceId, ReceiptEvidenceKind, ReceiptEvidenceRecordedV1, SourceMessageId,
        },
        shared_kernel::{CurrencyCode, Money, UserId},
    };
    use rust_decimal_macros::dec;
    let (verified, pool) = v2_test_support::fresh_v2_runtime().await;
    let recurring = moneykeeper::bootstrap::v2::supporting_contexts(&verified).recurring;
    let user = UserId::generate();
    let event = ReceiptEvidenceRecordedV1 {
        evidence_id: ReceiptEvidenceId::generate(),
        user_id: user,
        source_message_id: SourceMessageId::generate(),
        merchant: "Netflix".into(),
        kind: ReceiptEvidenceKind::Renewal,
        money: Some(Money::new(dec!(9.99), CurrencyCode::new("USD").unwrap(), 2).unwrap()),
        charged_at: Some(Utc::now()),
        parser_name: "netflix".into(),
        parser_version: 1,
        provenance_digest: [7; 32],
        recorded_at: Utc::now(),
    };
    let event_id = uuid::Uuid::new_v4();
    assert!(
        recurring
            .consume_mail_evidence(event_id, 1, event.clone())
            .await
            .unwrap()
            .applied
    );
    assert!(
        !recurring
            .consume_mail_evidence(event_id, 1, event)
            .await
            .unwrap()
            .applied
    );
    let row: (i64, i64) = sqlx::query_as(
        "SELECT count(*)::bigint,COALESCE(min(m.version),-1) FROM recurring.charge_evidence e JOIN recurring.charge_matching m ON m.evidence_id=e.id AND m.user_id=e.user_id WHERE e.user_id=$1",
    )
    .bind(user.into_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row, (1, 0));
}
