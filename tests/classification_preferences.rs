use chrono::{Duration, TimeZone, Utc};
use moneykeeper::bootstrap::build_contexts;
use moneykeeper::contexts::classification::public::{
    CategoryCatalog, CategoryKind, CategoryLifecycle, CreateCategoryNode, SetCategoryNodeLifecycle,
    UpdateCategoryNode,
};
use moneykeeper::contexts::preferences::public::Preferences;
use moneykeeper::shared_kernel::{CurrencyCode, IdempotencyKey, UserId};

#[path = "test_support.rs"]
mod test_support;

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 13, 12, 0, 0)
        .single()
        .unwrap()
}

fn key(value: &str) -> IdempotencyKey {
    IdempotencyKey::new(value).unwrap()
}

#[tokio::test]
async fn category_lifecycle_is_versioned_and_idempotent() {
    let database = test_support::fresh_database().await;
    let verified = database.initialize().await.unwrap();
    let categories = build_contexts(&verified).categories;
    let user_id = UserId::generate();
    let initial = categories.taxonomy(user_id, now()).await.unwrap();
    assert_eq!(initial.version, 1);
    assert_eq!(initial.starter_template_version, Some(1));

    let created = categories
        .create_node(
            CreateCategoryNode {
                user_id,
                idempotency_key: key("category-create"),
                expected_version: initial.version,
                name: "  Groceries  ".to_owned(),
                kind: CategoryKind::Expense,
                parent_id: None,
                position: None,
                color: None,
                icon: None,
            },
            now(),
        )
        .await
        .unwrap();
    assert_eq!(created.taxonomy_version, 2);
    assert_eq!(created.node.category.name, "Groceries");
    assert_eq!(created.node.category.version, 1);
    assert_eq!(created.node.category.lifecycle, CategoryLifecycle::Active);

    let renamed = categories
        .update_node(
            UpdateCategoryNode {
                user_id,
                idempotency_key: key("category-rename"),
                id: created.node.category.id,
                expected_version: created.taxonomy_version,
                name: Some("Food".to_owned()),
                color: None,
                icon: None,
            },
            now() + Duration::seconds(1),
        )
        .await
        .unwrap();
    assert_eq!(renamed.taxonomy_version, 3);
    assert_eq!(renamed.node.category.version, 2);

    let archive_command = SetCategoryNodeLifecycle {
        user_id,
        idempotency_key: key("category-archive"),
        id: created.node.category.id,
        expected_version: renamed.taxonomy_version,
        lifecycle: CategoryLifecycle::Archived,
    };
    let archived = categories
        .archive_node(archive_command.clone(), now() + Duration::seconds(2))
        .await
        .unwrap();
    assert_eq!(
        archived.node.category.lifecycle,
        CategoryLifecycle::Archived
    );
    assert_eq!(archived.taxonomy_version, 4);
    let repeated = categories
        .archive_node(archive_command, now() + Duration::seconds(3))
        .await
        .unwrap();
    assert_eq!(repeated, archived);

    let duplicate_transition = categories
        .archive_node(
            SetCategoryNodeLifecycle {
                user_id,
                idempotency_key: key("category-archive-again"),
                id: created.node.category.id,
                expected_version: archived.taxonomy_version,
                lifecycle: CategoryLifecycle::Archived,
            },
            now() + Duration::seconds(4),
        )
        .await
        .unwrap_err();
    assert!(duplicate_transition.is_lifecycle_conflict());

    let restore_command = SetCategoryNodeLifecycle {
        user_id,
        idempotency_key: key("category-restore"),
        id: created.node.category.id,
        expected_version: archived.taxonomy_version,
        lifecycle: CategoryLifecycle::Active,
    };
    let restored = categories
        .restore_node(restore_command.clone(), now() + Duration::seconds(5))
        .await
        .unwrap();
    assert_eq!(restored.node.category.lifecycle, CategoryLifecycle::Active);
    assert_eq!(restored.taxonomy_version, 5);
    let repeated = categories
        .restore_node(restore_command, now() + Duration::seconds(6))
        .await
        .unwrap();
    assert_eq!(repeated, restored);
}

#[tokio::test]
async fn category_conflicts_and_tenant_boundary_are_explicit() {
    let database = test_support::fresh_database().await;
    let verified = database.initialize().await.unwrap();
    let categories = build_contexts(&verified).categories;
    let owner = UserId::generate();
    let other_user = UserId::generate();
    let initial = categories.taxonomy(owner, now()).await.unwrap();
    let category = categories
        .create_node(
            CreateCategoryNode {
                user_id: owner,
                idempotency_key: key("owner-food"),
                expected_version: initial.version,
                name: "Food".to_owned(),
                kind: CategoryKind::Both,
                parent_id: None,
                position: None,
                color: None,
                icon: None,
            },
            now(),
        )
        .await
        .unwrap();

    let duplicate = categories
        .create_node(
            CreateCategoryNode {
                user_id: owner,
                idempotency_key: key("owner-food-duplicate"),
                expected_version: category.taxonomy_version,
                name: "fOoD".to_owned(),
                kind: CategoryKind::Expense,
                parent_id: None,
                position: None,
                color: None,
                icon: None,
            },
            now(),
        )
        .await
        .unwrap_err();
    assert!(duplicate.is_duplicate_name());

    let stale = categories
        .update_node(
            UpdateCategoryNode {
                user_id: owner,
                idempotency_key: key("owner-food-stale"),
                id: category.node.category.id,
                expected_version: 99,
                name: Some("Dining".to_owned()),
                color: None,
                icon: None,
            },
            now(),
        )
        .await
        .unwrap_err();
    assert!(stale.is_version_conflict());

    let invisible = categories
        .get_node(other_user, category.node.category.id, now())
        .await
        .unwrap_err();
    assert!(invisible.is_not_found());
    let other_taxonomy = categories.taxonomy(other_user, now()).await.unwrap();
    assert_eq!(other_taxonomy.starter_template_version, Some(1));
    assert!(!other_taxonomy.roots.is_empty());
    assert!(
        other_taxonomy
            .roots
            .iter()
            .all(|root| root.category.id != category.node.category.id)
    );
}

#[tokio::test]
async fn idempotent_category_command_is_fenced_by_the_stored_version() {
    let database = test_support::fresh_database().await;
    let verified = database.initialize().await.unwrap();
    let categories = build_contexts(&verified).categories;
    let user_id = UserId::generate();
    let initial = categories.taxonomy(user_id, now()).await.unwrap();
    let category = categories
        .create_node(
            CreateCategoryNode {
                user_id,
                idempotency_key: key("concurrency-create"),
                expected_version: initial.version,
                name: "Concurrency".to_owned(),
                kind: CategoryKind::Expense,
                parent_id: None,
                position: None,
                color: None,
                icon: None,
            },
            now(),
        )
        .await
        .unwrap();

    // Hold the aggregate row after the command has read version 2. Its CAS save
    // blocks, allowing this competing writer to advance the taxonomy first.
    let mut competing_writer = verified.begin().await.unwrap();
    sqlx::query(
        "SELECT user_id FROM classification.category_taxonomies \
         WHERE user_id = $1 FOR UPDATE",
    )
    .bind(user_id.into_uuid())
    .fetch_one(&mut *competing_writer)
    .await
    .unwrap();

    let no_op_categories = categories.clone();
    let category_id = category.node.category.id;
    let archive = tokio::spawn(async move {
        no_op_categories
            .archive_node(
                SetCategoryNodeLifecycle {
                    user_id,
                    idempotency_key: key("concurrency-archive"),
                    id: category_id,
                    expected_version: 2,
                    lifecycle: CategoryLifecycle::Archived,
                },
                now() + Duration::seconds(1),
            )
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
                      AND query LIKE '%UPDATE classification.category_taxonomies%'
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
    .expect("category command should reach the taxonomy CAS");

    let changed = sqlx::query(
        "UPDATE classification.category_taxonomies
         SET version = 3, updated_at = $1
         WHERE user_id = $2 AND version = 2",
    )
    .bind(now() + Duration::seconds(2))
    .bind(user_id.into_uuid())
    .execute(&mut *competing_writer)
    .await
    .unwrap();
    assert_eq!(changed.rows_affected(), 1);
    competing_writer.commit().await.unwrap();

    let error = archive.await.unwrap().unwrap_err();
    assert!(error.is_version_conflict());
    let current = categories
        .get_node(user_id, category.node.category.id, now())
        .await
        .unwrap();
    assert_eq!(current.node.category.lifecycle, CategoryLifecycle::Active);
    assert_eq!(current.taxonomy_version, 3);
}

#[tokio::test]
async fn preferences_default_and_compare_and_swap_are_tenant_scoped() {
    let database = test_support::fresh_database().await;
    let verified = database.initialize().await.unwrap();
    let contexts = build_contexts(&verified);
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
