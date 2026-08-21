use moneykeeper::contexts::sharing::public::*;
use moneykeeper::shared_kernel::UserId;

#[test]
fn domain_contact_normalizes_and_preserves_owner_through_archive() {
    let owner = UserId::generate();
    let mut contact = Contact::create(
        ContactId::generate(),
        owner,
        ContactName::new("  Alice   Smith ").unwrap(),
        Some("  colleague  ".into()),
    )
    .unwrap();
    assert_eq!(contact.name().as_str(), "Alice Smith");
    assert_eq!(contact.note(), Some("colleague"));
    assert_eq!(contact.user_id(), owner);
    contact.archive(ContactVersion(1)).unwrap();
    assert_eq!(contact.status(), ContactStatus::Archived);
    assert!(contact.ensure_selectable().is_err());
    assert!(matches!(
        contact.restore(ContactVersion(1)),
        Err(SharingError::VersionConflict { .. })
    ));
    contact.restore(ContactVersion(2)).unwrap();
    assert_eq!(contact.status(), ContactStatus::Active);
    assert_eq!(contact.user_id(), owner);
}

#[test]
fn domain_contact_rejects_empty_name() {
    assert!(ContactName::new(" \n ").is_err());
}
