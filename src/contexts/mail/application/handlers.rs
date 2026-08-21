//! Mail command orchestration.
use super::super::{
    domain::{ConnectionVersion, GmailConnectionId},
    public::MailFacade,
};
use crate::shared_kernel::UserId;
use chrono::Utc;
pub(crate) async fn disconnect(
    f: &MailFacade,
    user: UserId,
    id: GmailConnectionId,
    expected: ConnectionVersion,
) -> Result<Option<ConnectionVersion>, sqlx::Error> {
    f.store.disconnect(user, id, expected, Utc::now()).await
}
pub(crate) async fn resync(
    f: &MailFacade,
    user: UserId,
    id: GmailConnectionId,
    expected: ConnectionVersion,
) -> Result<Option<(uuid::Uuid, ConnectionVersion)>, sqlx::Error> {
    f.store.request_resync(user, id, expected, Utc::now()).await
}
