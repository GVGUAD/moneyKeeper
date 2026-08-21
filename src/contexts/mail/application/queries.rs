//! Mail read use cases.
use super::super::domain::GmailConnectionId;
use super::super::public::{ConnectionView, MailFacade};
use crate::shared_kernel::UserId;
pub(crate) async fn list(f: &MailFacade, user: UserId) -> Result<Vec<ConnectionView>, sqlx::Error> {
    f.store.list_connections(user).await
}
pub(crate) async fn get(
    f: &MailFacade,
    user: UserId,
    id: GmailConnectionId,
) -> Result<Option<ConnectionView>, sqlx::Error> {
    f.store.get_connection(user, id).await
}
