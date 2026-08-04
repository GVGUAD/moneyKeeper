use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use regex::Regex;
use rust_decimal::Decimal;
use std::str::FromStr;

use crate::domain::email::RawEmail;
use crate::domain::receipt_parser::{ParsedReceipt, ReceiptParser};
use crate::domain::subscription::{BillingPeriod, SubscriptionProvider};
use crate::domain::subscription_charge::ReceiptKind;

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
        from.to_ascii_lowercase()
            .contains("info@account.netflix.com")
    }

    fn parse(&self, email: &RawEmail) -> anyhow::Result<Option<ParsedReceipt>> {
        let body = match email.body_text.as_deref() {
            Some(b) => b,
            None => return Ok(None),
        };
        let plan_re = Regex::new(r"(?i)Plan:\s*(.+)").unwrap();
        let total_re = Regex::new(r"(?i)Total:\s*\$?([0-9]+(?:\.[0-9]{2})?)\s*([A-Z]{3})").unwrap();
        let date_re = Regex::new(r"(?i)Date:\s*([A-Za-z]+ [0-9]{1,2}, [0-9]{4})").unwrap();

        let plan = plan_re
            .captures(body)
            .and_then(|c| c.get(1).map(|m| m.as_str().trim().to_string()))
            .ok_or_else(|| anyhow::anyhow!("plan not found in Netflix receipt"))?;
        let total = total_re
            .captures(body)
            .ok_or_else(|| anyhow::anyhow!("total not found in Netflix receipt"))?;
        let amount = Decimal::from_str(total.get(1).unwrap().as_str())?;
        let currency = total.get(2).unwrap().as_str().to_string();
        let date_str = date_re
            .captures(body)
            .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
            .ok_or_else(|| anyhow::anyhow!("date not found in Netflix receipt"))?;
        let charged_at = parse_us_date(&date_str)?;

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
            kind: ReceiptKind::Renewal,
        }))
    }
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
            message_id: "<netflix-renewal-1>".to_string(),
            from: "Netflix <info@account.netflix.com>".to_string(),
            subject: "Your Netflix payment".to_string(),
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
        assert_eq!(r.kind, ReceiptKind::Renewal);
        assert_eq!(r.billing_period_hint, Some(BillingPeriod::Monthly));
    }

    #[test]
    fn returns_none_when_body_missing() {
        let p = NetflixParser::new();
        let mut e = fixture_email();
        e.body_text = None;
        assert!(p.parse(&e).unwrap().is_none());
    }
}
