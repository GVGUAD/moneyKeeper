//! Characterized receipt parsers owned by the Mail adapter boundary.
pub(crate) mod apple;
pub(crate) mod google_play;
pub(crate) mod netflix;

use chrono::{DateTime, Utc};
use regex::Regex;
use rust_decimal::Decimal;
use scraper::Html;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BillingPeriod {
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SubscriptionProvider {
    AppleAppStore,
    GooglePlay,
    Netflix,
}

#[derive(Clone, Debug)]
pub(crate) struct RawEmail {
    pub provider_message_id: String,
    pub rfc_message_id: Option<String>,
    pub from: String,
    pub subject: String,
    pub authentication_results: Vec<String>,
    pub received_at: DateTime<Utc>,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ParsedReceipt {
    pub provider: SubscriptionProvider,
    pub product_name: String,
    pub merchant_key: String,
    pub amount: Decimal,
    pub currency: String,
    pub charged_at: DateTime<Utc>,
    pub billing_period_hint: Option<BillingPeriod>,
}

pub(crate) trait ReceiptParser: Send + Sync {
    fn matches_sender(&self, from: &str) -> bool;
    fn parse(&self, email: &RawEmail) -> anyhow::Result<Option<ParsedReceipt>>;
}

pub(crate) struct ParsedBy {
    pub parser_name: &'static str,
    pub parser_version: i32,
    pub receipt: ParsedReceipt,
}
pub(crate) struct ParserRegistry {
    parsers: Vec<(&'static str, i32, Box<dyn ReceiptParser>)>,
}
impl ParserRegistry {
    pub(crate) fn default_set() -> Self {
        Self {
            parsers: vec![
                ("netflix", 1, Box::new(netflix::NetflixParser::new())),
                (
                    "google_play",
                    1,
                    Box::new(google_play::GooglePlayParser::new()),
                ),
                ("apple", 1, Box::new(apple::AppleParser::new())),
            ],
        }
    }
    pub(crate) fn find(&self, from: &str) -> Option<&dyn ReceiptParser> {
        self.parsers
            .iter()
            .find(|(_, _, parser)| parser.matches_sender(from))
            .map(|(_, _, parser)| parser.as_ref())
    }
    pub(crate) fn parse(&self, email: &RawEmail) -> anyhow::Result<Option<ParsedBy>> {
        let Some((name, version, parser)) = self
            .parsers
            .iter()
            .find(|(_, _, parser)| parser.matches_sender(&email.from))
        else {
            return Ok(None);
        };
        parser.parse(email).map(|receipt| {
            receipt.map(|receipt| ParsedBy {
                parser_name: name,
                parser_version: *version,
                receipt,
            })
        })
    }
}

fn normalized_mailbox(from: &str) -> String {
    let value = from.trim();
    if let (Some(start), Some(end)) = (value.rfind('<'), value.rfind('>'))
        && start < end
    {
        return value[start + 1..end].trim().to_ascii_lowercase();
    }
    value.to_ascii_lowercase()
}

fn html_visible_text(html: &str) -> String {
    let document = Html::parse_document(html);
    document
        .tree
        .nodes()
        .filter(|node| {
            !node.ancestors().any(|ancestor| {
                ancestor
                    .value()
                    .as_element()
                    .is_some_and(|element| matches!(element.name(), "head" | "script" | "style"))
            })
        })
        .filter_map(|node| node.value().as_text())
        .map(|text| text.trim())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_explicit_non_recurring(subject: &str, body_text: &str) -> bool {
    let subject = subject.to_ascii_lowercase();
    ["refund", "cancel", "one-time", "one time"]
        .iter()
        .any(|marker| subject.contains(marker))
        || non_recurring_status_regex().is_match(body_text)
}

fn non_recurring_status_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r"(?im)^(?:status|transaction type|order type|purchase type):\s*(?:refunded?|cancel(?:led|ed|ation)|one[- ]time(?:\s+(?:purchase|order))?)\s*$",
        )
        .expect("valid non-recurring status regex")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_finds_supported_senders_only() {
        let registry = ParserRegistry::default_set();
        assert!(registry.find("info@account.netflix.com").is_some());
        assert!(registry.find("googleplay-noreply@google.com").is_some());
        assert!(registry.find("no_reply@email.apple.com").is_some());
        assert!(registry.find("noreply@hulu.com").is_none());
    }

    #[test]
    fn footer_language_is_not_a_transaction_status() {
        assert!(!is_explicit_non_recurring(
            "Your receipt",
            "Manage or cancel your subscription. See our refund policy.",
        ));
        assert!(is_explicit_non_recurring(
            "Your receipt",
            "Status: Refunded",
        ));
    }

    #[test]
    fn visible_text_includes_adjacent_table_cells() {
        assert_eq!(
            html_visible_text(
                "<html><body><table><tr><td>App Name:</td><td>Example Pro</td></tr></table></body></html>",
            ),
            "App Name:\nExample Pro"
        );
    }
}
