use super::super::public::{RecurringFacade, SubscriptionView};
use crate::shared_kernel::UserId;
pub(crate) async fn subscriptions(
    f: &RecurringFacade,
    user: UserId,
) -> Result<Vec<SubscriptionView>, sqlx::Error> {
    f.store.list_subscriptions(user).await
}
