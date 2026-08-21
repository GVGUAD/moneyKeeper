use chrono::{TimeZone, Utc};
use moneykeeper::{
    contexts::mail::domain::{
        ConnectionState, ConnectionVersion, EncryptedSecret, GmailConnection,
    },
    shared_kernel::UserId,
};

#[test]
fn credentials_are_redacted_and_connection_changes_fence_workers() {
    let now = Utc.with_ymd_and_hms(2026, 8, 20, 12, 0, 0).unwrap();
    let secret = EncryptedSecret::new("key-1", vec![7; 12], vec![1, 2, 3]).unwrap();
    assert!(!format!("{secret:?}").contains("1, 2, 3"));
    let mut connection = GmailConnection::connect(UserId::generate(), secret, now);
    assert_eq!(connection.state(), ConnectionState::Active);
    let first = connection.credential_generation();
    connection
        .request_resync(ConnectionVersion::INITIAL, now)
        .unwrap();
    let version = connection.version();
    connection.disconnect(version, now).unwrap();
    assert_eq!(connection.state(), ConnectionState::Disconnected);
    assert!(connection.credential_generation() > first);
    assert!(connection.credential().is_none());
}
