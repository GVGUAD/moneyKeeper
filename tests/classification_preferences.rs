use chrono::{Duration, TimeZone, Utc};
use moneykeeper::bootstrap::v2::supporting_contexts;
use moneykeeper::contexts::classification::public::{
    CategoryCatalog, CategoryCommand, CategoryKind, CategoryLifecycle,
};
use moneykeeper::contexts::preferences::public::Preferences;
use moneykeeper::shared_kernel::{CurrencyCode, UserId};

#[path = "v2_test_support.rs"]
mod v2_test_support;

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 13, 12, 0, 0)
        .single()
        .unwrap()
}

#[tokio::test]
async fn category_lifecycle_is_versioned_and_idempotent() {
    let database = v2_test_support::fresh_v2_database().await;
    let verified = database.initialize().await.unwrap();
    let categories = supporting_contexts(&verified).categories;
    let user_id = UserId::generate();
    let created = categories
        .create(
            CategoryCommand {
                user_id,
                name: "  Groceries  ".to_owned(),
                kind: CategoryKind::Expense,
            },
            now(),
        )
        .await
        .unwrap();
    assert_eq!(created.name, "Groceries");
    assert_eq!(created.version, 1);
    assert_eq!(created.lifecycle, CategoryLifecycle::Active);

    let renamed = categories
        .rename(
            user_id,
            created.id,
            "Food".to_owned(),
            1,
            now() + Duration::seconds(1),
        )
        .await
        .unwrap();
    assert_eq!(renamed.version, 2);

    let archived = categories
        .archive(user_id, created.id, 2, now() + Duration::seconds(2))
        .await
        .unwrap();
    assert_eq!(archived.lifecycle, CategoryLifecycle::Archived);
    assert_eq!(archived.version, 3);
    let repeated = categories
        .archive(user_id, created.id, 3, now() + Duration::seconds(3))
        .await
        .unwrap();
    assert_eq!(repeated.version, 3);

    let restored = categories
        .restore(user_id, created.id, 3, now() + Duration::seconds(4))
        .await
        .unwrap();
    assert_eq!(restored.lifecycle, CategoryLifecycle::Active);
    assert_eq!(restored.version, 4);
    let repeated = categories
        .restore(user_id, created.id, 4, now() + Duration::seconds(5))
        .await
        .unwrap();
    assert_eq!(repeated.version, 4);
}

#[tokio::test]
async fn category_conflicts_and_tenant_boundary_are_explicit() {
    let database = v2_test_support::fresh_v2_database().await;
    let verified = database.initialize().await.unwrap();
    let categories = supporting_contexts(&verified).categories;
    let owner = UserId::generate();
    let other_user = UserId::generate();
    let category = categories
        .create(
            CategoryCommand {
                user_id: owner,
                name: "Food".to_owned(),
                kind: CategoryKind::Both,
            },
            now(),
        )
        .await
        .unwrap();

    let duplicate = categories
        .create(
            CategoryCommand {
                user_id: owner,
                name: "fOoD".to_owned(),
                kind: CategoryKind::Expense,
            },
            now(),
        )
        .await
        .unwrap_err();
    assert!(duplicate.is_duplicate_name());

    let stale = categories
        .rename(owner, category.id, "Dining".to_owned(), 99, now())
        .await
        .unwrap_err();
    assert!(stale.is_version_conflict());

    let invisible = categories.get(other_user, category.id).await.unwrap_err();
    assert!(invisible.is_not_found());
    assert!(categories.list(other_user).await.unwrap().is_empty());
}

#[tokio::test]
async fn idempotent_category_command_is_fenced_by_the_stored_version() {
    let database = v2_test_support::fresh_v2_database().await;
    let verified = database.initialize().await.unwrap();
    let categories = supporting_contexts(&verified).categories;
    let user_id = UserId::generate();
    let category = categories
        .create(
            CategoryCommand {
                user_id,
                name: "Concurrency".to_owned(),
                kind: CategoryKind::Expense,
            },
            now(),
        )
        .await
        .unwrap();
    categories
        .archive(user_id, category.id, 1, now() + Duration::seconds(1))
        .await
        .unwrap();

    // Hold the row lock after the no-op command has read version 2. Its CAS
    // update will block, allowing this competing writer to advance the stored
    // version before the no-op resumes.
    let mut competing_writer = verified.begin().await.unwrap();
    sqlx::query(
        "SELECT id FROM classification.categories \
         WHERE id = $1 AND user_id = $2 FOR UPDATE",
    )
    .bind(category.id.into_uuid())
    .bind(user_id.into_uuid())
    .fetch_one(&mut *competing_writer)
    .await
    .unwrap();

    let no_op_categories = categories.clone();
    let category_id = category.id;
    let no_op = tokio::spawn(async move {
        no_op_categories
            .archive(user_id, category_id, 2, now() + Duration::seconds(2))
            .await
    });

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let mut observer = verified.acquire().await.unwrap();
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (
                    SELECT 1 FROM pg_stat_activity
                    WHERE pid <> pg_backend_pid()
                      AND datname = current_database()
                      AND state = 'active'
                      AND wait_event_type = 'Lock'
                      AND query LIKE '%UPDATE classification.categories%'
                 )",
            )
            .fetch_one(&mut *observer)
            .await
            .unwrap();
            if waiting {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("idempotent command should reach the row-level CAS");

    let changed = sqlx::query(
        "UPDATE classification.categories
         SET lifecycle = 'active', version = 3, updated_at = $1
         WHERE id = $2 AND user_id = $3 AND version = 2",
    )
    .bind(now() + Duration::seconds(3))
    .bind(category.id.into_uuid())
    .bind(user_id.into_uuid())
    .execute(&mut *competing_writer)
    .await
    .unwrap();
    assert_eq!(changed.rows_affected(), 1);
    competing_writer.commit().await.unwrap();

    let error = no_op.await.unwrap().unwrap_err();
    assert!(error.is_version_conflict());
    let current = categories.get(user_id, category.id).await.unwrap();
    assert_eq!(current.lifecycle, CategoryLifecycle::Active);
    assert_eq!(current.version, 3);
}

#[tokio::test]
async fn preferences_default_and_compare_and_swap_are_tenant_scoped() {
    let database = v2_test_support::fresh_v2_database().await;
    let verified = database.initialize().await.unwrap();
    let contexts = supporting_contexts(&verified);
    let preferences = contexts.preferences;
    let currencies = contexts.currencies;
    let user_id = UserId::generate();
    let other_user = UserId::generate();

    let default = preferences.get(user_id, now()).await.unwrap();
    assert_eq!(default.base_currency.as_str(), "UAH");
    assert_eq!(default.version, 0);
    assert!(!default.persisted);

    let created = preferences
        .set_base_currency(
            &currencies,
            user_id,
            CurrencyCode::new("USD").unwrap(),
            0,
            now() + Duration::seconds(1),
        )
        .await
        .unwrap();
    assert_eq!(created.base_currency.as_str(), "USD");
    assert_eq!(created.version, 1);
    assert!(created.persisted);

    let stale = preferences
        .set_base_currency(
            &currencies,
            user_id,
            CurrencyCode::new("EUR").unwrap(),
            0,
            now() + Duration::seconds(2),
        )
        .await
        .unwrap_err();
    assert!(stale.is_version_conflict());

    let other_default = preferences.get(other_user, now()).await.unwrap();
    assert_eq!(other_default.base_currency.as_str(), "UAH");
    assert!(!other_default.persisted);
}
