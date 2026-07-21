use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use regex::Regex;
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::OnceLock;

use super::{is_explicit_non_recurring, normalized_mailbox};
use crate::domain::email::RawEmail;
use crate::domain::receipt_parser::{ParsedReceipt, ReceiptParser};
use crate::domain::subscription::{BillingPeriod, SubscriptionProvider};

pub struct NetflixParser;

impl NetflixParser {
    pub fn new() -> Self {
        Self
    }
}

impl Default for NetflixParser {
    fn default() -> Self {
        Self::new()
    }
}

impl ReceiptParser for NetflixParser {
    fn matches_sender(&self, from: &str) -> bool {
        normalized_mailbox(from) == "info@account.netflix.com"
    }

    fn parse(&self, email: &RawEmail) -> anyhow::Result<Option<ParsedReceipt>> {
        let body = match email.body_text.as_deref() {
            Some(b) => b,
            None => return Ok(None),
        };
        let plan = plan_regex()
            .captures(body)
            .and_then(|c| c.get(1).map(|m| m.as_str().trim().to_string()))
            .filter(|value| !value.is_empty());
        let Some(plan) = plan else { return Ok(None) };
        let Some(total) = total_regex().captures(body) else {
            return Ok(None);
        };
        let amount = Decimal::from_str(&total.get(1).unwrap().as_str().replace(',', "."))?;
        if amount <= Decimal::ZERO {
            return Ok(None);
        }
        let currency = total.get(2).unwrap().as_str().to_ascii_uppercase();
        let date_str = date_regex()
            .captures(body)
            .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
            .ok_or_else(|| anyhow::anyhow!("invalid Netflix recurring receipt date"))?;
        let charged_at = parse_us_date(&date_str)?;
        if is_explicit_non_recurring(&email.subject, body) {
            return Ok(None);
        }

        let merchant_key = format!(
            "netflix.com:{}",
            plan.to_ascii_lowercase().replace(' ', "_")
        );

        Ok(Some(ParsedReceipt {
            provider: SubscriptionProvider::Netflix,
            product_name: plan,
            merchant_key,
            amount,
            currency,
            charged_at,
            billing_period_hint: Some(BillingPeriod::Monthly),
        }))
    }
}

fn plan_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"(?im)^Plan:\s*(.+)$").expect("valid Netflix plan regex"))
}

fn total_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?i)Total:\s*(?:\$|€|£)?\s*([0-9]+(?:[.,][0-9]{2})?)\s*([A-Z]{3})")
            .expect("valid Netflix total regex")
    })
}

fn date_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?i)Date:\s*([A-Za-z]+ [0-9]{1,2}, [0-9]{4})")
            .expect("valid Netflix date regex")
    })
}

fn parse_us_date(s: &str) -> anyhow::Result<DateTime<Utc>> {
    let naive = NaiveDate::parse_from_str(s, "%B %d, %Y")?;
    let dt = naive.and_hms_opt(0, 0, 0).unwrap();
    Ok(Utc.from_utc_datetime(&dt))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_email() -> RawEmail {
        let body = std::fs::read_to_string("tests/fixtures/receipts/netflix/renewal.txt").unwrap();
        RawEmail {
            provider_message_id: "gmail-netflix-1".to_string(),
            rfc_message_id: Some("<netflix-renewal-1>".to_string()),
            from: "Netflix <info@account.netflix.com>".to_string(),
            subject: "Your Netflix payment".to_string(),
            authentication_results: vec![],
            received_at: Utc::now(),
            body_text: Some(body),
            body_html: None,
        }
    }

    #[test]
    fn matches_sender_case_insensitive() {
        let p = NetflixParser::new();
        assert!(p.matches_sender("Netflix <info@account.netflix.com>"));
        assert!(p.matches_sender("INFO@ACCOUNT.NETFLIX.COM"));
        assert!(!p.matches_sender("noreply@hulu.com"));
    }

    #[test]
    fn parses_renewal_fixture() {
        let p = NetflixParser::new();
        let r = p.parse(&fixture_email()).unwrap().unwrap();
        assert_eq!(r.provider, SubscriptionProvider::Netflix);
        assert_eq!(r.product_name, "Netflix Premium");
        assert_eq!(r.merchant_key, "netflix.com:netflix_premium");
        assert_eq!(r.amount.to_string(), "15.99");
        assert_eq!(r.currency, "USD");
        assert_eq!(r.billing_period_hint, Some(BillingPeriod::Monthly));
    }

    #[test]
    fn recurring_footer_does_not_turn_receipt_into_cancellation() {
        let receipt = NetflixParser::new()
            .parse(&fixture_email())
            .unwrap()
            .unwrap();
        assert_eq!(receipt.amount.to_string(), "15.99");
    }

    #[test]
    fn ignores_explicit_cancellation_fixture() {
        let mut email = fixture_email();
        email.subject = "Your Netflix subscription cancellation".to_string();
        email.body_text = Some(
            std::fs::read_to_string("tests/fixtures/receipts/netflix/cancellation.txt").unwrap(),
        );
        assert!(NetflixParser::new().parse(&email).unwrap().is_none());
    }

    #[test]
    fn returns_none_when_body_missing() {
        let p = NetflixParser::new();
        let mut e = fixture_email();
        e.body_text = None;
        assert!(p.parse(&e).unwrap().is_none());
    }

    #[test]
    fn ignores_non_recurring_message_from_netflix() {
        let p = NetflixParser::new();
        let mut email = fixture_email();
        email.subject = "New films this week".into();
        email.body_text = Some("Stream something new tonight".into());
        assert!(p.parse(&email).unwrap().is_none());
    }

    #[test]
    fn sender_match_is_not_substring_based() {
        let p = NetflixParser::new();
        assert!(!p.matches_sender("info@account.netflix.com.evil.test"));
    }
}
