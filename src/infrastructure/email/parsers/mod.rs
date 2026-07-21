pub mod apple;
pub mod google_play;
pub mod netflix;

use regex::Regex;
use scraper::Html;
use std::sync::OnceLock;

use crate::domain::receipt_parser::ReceiptParser;

pub struct ParserRegistry {
    parsers: Vec<Box<dyn ReceiptParser>>,
}

impl ParserRegistry {
    pub fn default_set() -> Self {
        Self {
            parsers: vec![
                Box::new(netflix::NetflixParser::new()),
                Box::new(google_play::GooglePlayParser::new()),
                Box::new(apple::AppleParser::new()),
            ],
        }
    }

    pub fn find(&self, from: &str) -> Option<&dyn ReceiptParser> {
        self.parsers
            .iter()
            .find(|p| p.matches_sender(from))
            .map(|b| b.as_ref())
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

/// Provider receipts are commonly table/div based. Reading every visible text
/// node keeps field labels and values available even when they are in adjacent
/// cells, while excluding markup, scripts, and style attributes.
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

/// Reject explicit non-recurring notifications without treating ordinary
/// receipt footers (for example, "cancel any time" or a refund policy) as the
/// message's transaction status.
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
mod registry_tests {
    use super::*;

    #[test]
    fn finds_netflix() {
        let reg = ParserRegistry::default_set();
        assert!(reg.find("info@account.netflix.com").is_some());
    }

    #[test]
    fn finds_google_play() {
        let reg = ParserRegistry::default_set();
        assert!(reg.find("googleplay-noreply@google.com").is_some());
    }

    #[test]
    fn finds_none_for_unknown() {
        let reg = ParserRegistry::default_set();
        assert!(reg.find("noreply@hulu.com").is_none());
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
