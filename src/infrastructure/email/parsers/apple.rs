use chrono::{NaiveDate, TimeZone, Utc};
use regex::Regex;
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::OnceLock;

use crate::domain::email::RawEmail;
use crate::domain::receipt_parser::{ParsedReceipt, ReceiptParser};
use crate::domain::subscription::{BillingPeriod, SubscriptionProvider};

use super::{html_visible_text, is_explicit_non_recurring, normalized_mailbox};

pub struct AppleParser;

impl AppleParser {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AppleParser {
    fn default() -> Self {
        Self::new()
    }
}

impl ReceiptParser for AppleParser {
    fn matches_sender(&self, from: &str) -> bool {
        normalized_mailbox(from) == "no_reply@email.apple.com"
    }

    fn parse(&self, email: &RawEmail) -> anyhow::Result<Option<ParsedReceipt>> {
        let html = match email.body_html.as_deref() {
            Some(h) => h,
            None => return Ok(None),
        };
        let text = html_visible_text(html);

        let Some(product_name) = name_regex()
            .captures(&text)
            .and_then(|c| c.get(1).map(|m| m.as_str().trim().to_string()))
            .filter(|value| !value.is_empty())
        else {
            return Ok(None);
        };

        let period_str = subscription_regex()
            .captures(&text)
            .and_then(|c| c.get(1).map(|m| m.as_str().to_ascii_lowercase()))
            .unwrap_or_default();
        let billing_period_hint = match period_str.as_str() {
            "monthly" => Some(BillingPeriod::Monthly),
            "yearly" | "annual" => Some(BillingPeriod::Yearly),
            "weekly" => Some(BillingPeriod::Weekly),
            _ => None,
        };
        if billing_period_hint.is_none() {
            return Ok(None);
        }

        let Some(amount_caps) = amount_regex().captures(&text) else {
            return Ok(None);
        };
        let amount_str = amount_caps.get(2).unwrap().as_str();
        let amount = Decimal::from_str(&amount_str.replace(',', "."))?;
        if amount <= Decimal::ZERO {
            return Ok(None);
        }
        let Some(currency) = normalize_currency(amount_caps.get(1).map(|m| m.as_str())) else {
            return Ok(None);
        };

        let date_str = date_regex()
            .captures(&text)
            .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
            .ok_or_else(|| anyhow::anyhow!("date not found in Apple receipt"))?;
        let naive = NaiveDate::parse_from_str(&date_str, "%B %d, %Y")?;
        let charged_at = Utc.from_utc_datetime(&naive.and_hms_opt(0, 0, 0).unwrap());
        if is_explicit_non_recurring(&email.subject, &text) {
            return Ok(None);
        }

        let merchant_key = format!(
            "apps.apple.com:{}",
            product_name.to_ascii_lowercase().replace(' ', "_")
        );

        Ok(Some(ParsedReceipt {
            provider: SubscriptionProvider::AppleAppStore,
            product_name,
            merchant_key,
            amount,
            currency,
            charged_at,
            billing_period_hint,
        }))
    }
}

fn name_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"(?im)^App Name:\s*(.+)$").expect("valid Apple name regex"))
}

fn subscription_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?im)^Subscription:\s*(weekly|monthly|yearly|annual)\s*$")
            .expect("valid Apple subscription regex")
    })
}

fn amount_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?i)Amount Billed:\s*(\$|€|£|[A-Z]{3})?\s*([0-9]+(?:[.,][0-9]{1,2})?)")
            .expect("valid Apple amount regex")
    })
}

fn date_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?i)Receipt Date:\s*([A-Za-z]+ [0-9]{1,2}, [0-9]{4})")
            .expect("valid Apple date regex")
    })
}

fn normalize_currency(value: Option<&str>) -> Option<String> {
    match value?.trim().to_ascii_uppercase().as_str() {
        "$" => Some("USD".to_string()),
        "€" => Some("EUR".to_string()),
        "£" => Some("GBP".to_string()),
        code if code.len() == 3 && code.bytes().all(|b| b.is_ascii_alphabetic()) => {
            Some(code.to_string())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_email() -> RawEmail {
        let html = std::fs::read_to_string("tests/fixtures/receipts/apple/renewal.html").unwrap();
        RawEmail {
            provider_message_id: "gmail-apple-1".to_string(),
            rfc_message_id: Some("<apple-1>".to_string()),
            from: "Apple <no_reply@email.apple.com>".to_string(),
            subject: "Your receipt from Apple".to_string(),
            authentication_results: vec![],
            received_at: Utc::now(),
            body_text: None,
            body_html: Some(html),
        }
    }

    #[test]
    fn matches_sender() {
        let p = AppleParser::new();
        assert!(p.matches_sender("Apple <no_reply@email.apple.com>"));
        assert!(!p.matches_sender("info@account.netflix.com"));
    }

    #[test]
    fn parses_renewal_fixture() {
        let p = AppleParser::new();
        let r = p.parse(&fixture_email()).unwrap().unwrap();
        assert_eq!(r.provider, SubscriptionProvider::AppleAppStore);
        assert!(r.product_name.contains("iCloud"));
        assert_eq!(r.amount.to_string(), "0.99");
        assert_eq!(r.currency, "USD");
        assert_eq!(r.billing_period_hint, Some(BillingPeriod::Monthly));
    }

    #[test]
    fn returns_none_when_no_html() {
        let p = AppleParser::new();
        let email = RawEmail {
            provider_message_id: "gmail-apple-2".to_string(),
            rfc_message_id: Some("<apple-2>".to_string()),
            from: "Apple <no_reply@email.apple.com>".to_string(),
            subject: "Your receipt from Apple".to_string(),
            authentication_results: vec![],
            received_at: Utc::now(),
            body_text: Some("some text".to_string()),
            body_html: None,
        };
        assert!(p.parse(&email).unwrap().is_none());
    }

    #[test]
    fn ignores_cancellation_and_spoofed_sender() {
        let p = AppleParser::new();
        let mut email = fixture_email();
        email.subject = "Your subscription cancellation".into();
        assert!(p.parse(&email).unwrap().is_none());
        assert!(!p.matches_sender("no_reply@email.apple.com.evil.test"));
    }

    #[test]
    fn recurring_footer_does_not_turn_receipt_into_refund() {
        assert!(
            AppleParser::new()
                .parse(&fixture_email())
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn parses_table_based_renewal_fixture() {
        let mut email = fixture_email();
        email.body_html = Some(
            std::fs::read_to_string("tests/fixtures/receipts/apple/table_renewal.html").unwrap(),
        );
        let receipt = AppleParser::new().parse(&email).unwrap().unwrap();
        assert_eq!(receipt.product_name, "iCloud+ 200GB");
        assert_eq!(receipt.amount.to_string(), "2.99");
        assert_eq!(receipt.currency, "USD");
    }

    #[test]
    fn ignores_explicit_refund_fixture() {
        let mut email = fixture_email();
        email.subject = "Your Apple refund".to_string();
        email.body_html =
            Some(std::fs::read_to_string("tests/fixtures/receipts/apple/refund.html").unwrap());
        assert!(AppleParser::new().parse(&email).unwrap().is_none());
    }
}
