use chrono::Utc;
use moneykeeper::{
    domain::{email::RawEmail, receipt_parser::ReceiptParser},
    infrastructure::email::parsers::{
        apple::AppleParser, google_play::GooglePlayParser, netflix::NetflixParser,
    },
};
#[test]
fn parser_google_play_fixture_is_preserved() {
    let html = std::fs::read_to_string("tests/fixtures/receipts/google_play/renewal.html").unwrap();
    let email = RawEmail {
        provider_message_id: "v2-gp".into(),
        rfc_message_id: None,
        from: "googleplay-noreply@google.com".into(),
        subject: "Your Google Play Order Receipt".into(),
        authentication_results: vec![],
        received_at: Utc::now(),
        body_text: None,
        body_html: Some(html),
    };
    assert!(GooglePlayParser::new().parse(&email).unwrap().is_some());
}
#[test]
fn parser_apple_and_netflix_fixtures_are_preserved() {
    let apple = RawEmail {
        provider_message_id: "v2-apple".into(),
        rfc_message_id: None,
        from: "no_reply@email.apple.com".into(),
        subject: "Your receipt from Apple".into(),
        authentication_results: vec![],
        received_at: Utc::now(),
        body_text: None,
        body_html: Some(
            std::fs::read_to_string("tests/fixtures/receipts/apple/renewal.html").unwrap(),
        ),
    };
    assert!(AppleParser::new().parse(&apple).unwrap().is_some());
    let netflix = RawEmail {
        provider_message_id: "v2-netflix".into(),
        rfc_message_id: None,
        from: "info@account.netflix.com".into(),
        subject: "Your Netflix payment".into(),
        authentication_results: vec![],
        received_at: Utc::now(),
        body_text: Some(
            std::fs::read_to_string("tests/fixtures/receipts/netflix/renewal.txt").unwrap(),
        ),
        body_html: None,
    };
    assert!(NetflixParser::new().parse(&netflix).unwrap().is_some());
}
