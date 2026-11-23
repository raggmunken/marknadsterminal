//! A rule based extractor with no model behind it.
//!
//! It exists for two reasons. It lets the whole chain run and be tested
//! without an API key, and it is the bar the language model has to beat
//! in the evaluation. It only understands a few fixed phrasings.

use crate::{quote, ExtractError, Extractor, RawRelation};
use regex::Regex;
use std::collections::BTreeSet;
use std::sync::LazyLock;
use term_core::{Amount, Direction, Unit};

/// A company name: capitalised words, optionally ending in a short legal suffix.
/// A trailing full stop is only accepted after such a suffix, so that a
/// name does not run into the next sentence.
const NAME: &str =
    r"[A-Z][A-Za-z0-9&'\-]*(?: (?:of |de )?[A-Z][A-Za-z0-9&'\-]*)*(?:,? (?:Inc|Corp|Co|Ltd)\.)?";

static CUSTOMER_SHARE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?P<name>{NAME}),? (?:accounted for|represented|comprised) (?:approximately |about |roughly )?(?P<pct>\d{{1,3}}(?:\.\d+)?) ?(?:%|percent) of (?:our |the Company's )?(?P<basis>(?:total |consolidated |net )*(?:sales|revenues?|accounts receivable|trade receivables))"
    ))
    .expect("customer pattern compiles")
});

static SUPPLIER_ROLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?P<name>{NAME}),? (?:is|remains|was) (?:our|the Company's) (?:sole|single|only|primary|principal) (?:source|supplier|manufacturer)"
    ))
    .expect("supplier role pattern compiles")
});

static SUPPLIER_VERB: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?:purchases? from|supplied by|sourced from|manufactured by) (?P<name>{NAME})"
    ))
    .expect("supplier verb pattern compiles")
});

static YEAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:19|20)\d{2}\b").expect("year pattern compiles"));

static ANONYMOUS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:customer|distributor|supplier|vendor|company) [a-z0-9]$")
        .expect("anonymous pattern compiles")
});

/// Capitalised words that start sentences but are not companies.
const NOT_A_NAME: &[&str] = &[
    "we", "it", "they", "which", "that", "this", "these", "those", "no", "none", "one", "two",
    "three", "the company", "company", "customers", "sales", "revenue", "revenues", "each",
    "together", "collectively", "none of our customers", "no single customer", "no customer",
];

#[derive(Debug, Default, Clone, Copy)]
pub struct BaselineExtractor;

impl BaselineExtractor {
    pub fn extract_sync(&self, passage: &str) -> Vec<RawRelation> {
        // Work on the normalised text so that typographic quotes do not
        // break the patterns. The quote check normalises the same way.
        let text = quote::normalize(passage);
        let fiscal_year = single_year(&text);
        let mut out = Vec::new();

        for caps in CUSTOMER_SHARE.captures_iter(&text) {
            let whole = caps.get(0).expect("group 0 always exists");
            let Some(name) = clean_name(&caps["name"]) else { continue };
            // "A and B accounted for 12% and 10%": the pattern would pair
            // the wrong name with the wrong number, so leave it alone.
            if text[..whole.start()].trim_end().ends_with(" and") {
                continue;
            }
            let Ok(value) = caps["pct"].parse::<f64>() else { continue };
            if value <= 0.0 || value > 100.0 {
                continue;
            }
            out.push(RawRelation {
                counterparty: name,
                direction: Direction::Customer,
                amount: Some(Amount {
                    value,
                    unit: Unit::Percent,
                    basis: Some(caps["basis"].to_string()),
                }),
                fiscal_year,
                quote: whole.as_str().to_string(),
            });
        }

        for pattern in [&*SUPPLIER_ROLE, &*SUPPLIER_VERB] {
            for caps in pattern.captures_iter(&text) {
                let whole = caps.get(0).expect("group 0 always exists");
                let Some(name) = clean_name(&caps["name"]) else { continue };
                out.push(RawRelation {
                    counterparty: name,
                    direction: Direction::Supplier,
                    amount: None,
                    fiscal_year,
                    quote: whole.as_str().to_string(),
                });
            }
        }
        out
    }
}

impl Extractor for BaselineExtractor {
    fn name(&self) -> &str {
        "baseline"
    }

    async fn extract(&self, passage: &str) -> Result<Vec<RawRelation>, ExtractError> {
        Ok(self.extract_sync(passage))
    }
}

fn clean_name(raw: &str) -> Option<String> {
    let name = raw.trim().trim_end_matches([',', '.']).trim();
    if name.chars().count() < 3 {
        return None;
    }
    if NOT_A_NAME.contains(&name.to_lowercase().as_str()) || ANONYMOUS.is_match(name) {
        return None;
    }
    Some(name.to_string())
}

/// The fiscal year, but only when the passage mentions exactly one year.
fn single_year(text: &str) -> Option<u16> {
    let years: BTreeSet<u16> = YEAR
        .find_iter(text)
        .filter_map(|m| m.as_str().parse().ok())
        .collect();
    if years.len() == 1 {
        years.into_iter().next()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str) -> Vec<RawRelation> {
        BaselineExtractor.extract_sync(text)
    }

    #[test]
    fn finds_customer_with_share() {
        let found = run("In fiscal 2025, Example Motors Corp. accounted for approximately 21% of our net sales.");
        assert_eq!(found.len(), 1);
        let r = &found[0];
        assert_eq!(r.counterparty, "Example Motors Corp");
        assert_eq!(r.direction, Direction::Customer);
        assert_eq!(r.fiscal_year, Some(2025));
        let amount = r.amount.as_ref().unwrap();
        assert_eq!(amount.value, 21.0);
        assert_eq!(amount.basis.as_deref(), Some("net sales"));
        assert_eq!(r.quote, "Example Motors Corp. accounted for approximately 21% of our net sales");
    }

    #[test]
    fn handles_typographic_apostrophe_and_decimal() {
        let found = run("Sales to Brightline Retail Group represented 12.5 percent of the Company\u{2019}s consolidated revenues.");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].counterparty, "Brightline Retail Group");
        assert_eq!(found[0].amount.as_ref().unwrap().value, 12.5);
    }

    #[test]
    fn skips_anonymous_and_generic_subjects() {
        assert!(run("Customer A accounted for 14% of our net sales in 2025.").is_empty());
        assert!(run("No single customer accounted for 10% of our revenues.").is_empty());
        assert!(run("We accounted for 10% of our revenues.").is_empty());
    }

    #[test]
    fn skips_ambiguous_pairs() {
        let found = run("Alpha Stores Inc. and Beta Markets Inc. accounted for 12% and 10% of net sales.");
        assert!(found.is_empty(), "got {found:?}");
    }

    #[test]
    fn finds_sole_source_supplier() {
        let found = run("Kestrel Semiconductor Ltd. is our sole source for the controller chip.");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].counterparty, "Kestrel Semiconductor Ltd");
        assert_eq!(found[0].direction, Direction::Supplier);
        assert!(found[0].amount.is_none());
    }

    #[test]
    fn supplier_name_stops_at_sentence_end() {
        let found = run("Our housings are supplied by Polar Castings Ltd. Any disruption would hurt us.");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].counterparty, "Polar Castings Ltd");
    }

    #[test]
    fn year_is_dropped_when_passage_has_two() {
        let found = run("Example Motors Corp. accounted for 21% of net sales in 2025, up from 2024.");
        assert_eq!(found[0].fiscal_year, None);
    }

    #[test]
    fn every_quote_is_verbatim_in_the_passage() {
        let passage = "During 2025, Example Motors Corp. accounted for 21% of our net sales, and parts were supplied by Polar Castings Ltd.";
        let found = run(passage);
        assert_eq!(found.len(), 2);
        for r in found {
            assert_eq!(quote::verify_quote(passage, &r.quote), Ok(()), "quote {:?}", r.quote);
        }
    }
}
