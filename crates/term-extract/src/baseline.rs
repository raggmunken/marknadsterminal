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
        r"(?P<name>{NAME})(?:, [^,.;]{{3,140}},)?,? (?:accounted for|represented|comprised) (?:approximately |about |roughly )?(?P<pct>\d{{1,3}}(?:\.\d+)?) ?(?:%|percent) of (?:our |the Company's )?(?P<basis>(?:total |consolidated |net )*(?:sales|revenues?|accounts receivable|trade receivables))"
    ))
    .expect("customer pattern compiles")
});

/// Two customers in one sentence: "A and B accounted for 12% and 10% of sales".
/// The numbers are paired with the names in the order they are written.
static CUSTOMER_PAIR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?P<a>{NAME}),? and (?P<b>{NAME}),? (?:accounted for|represented|comprised) (?:approximately |about |roughly )?(?P<p1>\d{{1,3}}(?:\.\d+)?) ?(?:%|percent),? and (?:approximately |about |roughly )?(?P<p2>\d{{1,3}}(?:\.\d+)?) ?(?:%|percent)(?:,? respectively,?)? of (?:our |the Company's )?(?P<basis>(?:total |consolidated |net )*(?:sales|revenues?|accounts receivable|trade receivables))"
    ))
    .expect("pair pattern compiles")
});

/// "net sales to General Motors Company (including its subsidiaries) were approximately 18%".
static SALES_TO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?P<basis>(?:total |consolidated |net |worldwide )*(?:sales|revenues?)) to (?P<name>{NAME})(?: \([^)]{{0,60}}\))?[^.%]{{0,120}}? (?:were|was|represented|accounted for) (?:approximately |about |roughly )?(?P<pct>\d{{1,3}}(?:\.\d+)?) ?(?:%|percent)"
    ))
    .expect("sales-to pattern compiles")
});

/// "our top five customers accounted for 37% of net sales, with General Motors Company representing 18%".
static NAME_REPRESENTING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?P<name>{NAME}),? representing (?:approximately |about |roughly )?(?P<pct>\d{{1,3}}(?:\.\d+)?) ?(?:%|percent)"
    ))
    .expect("representing pattern compiles")
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

        for (pattern, has_basis) in [(&*SALES_TO, true), (&*NAME_REPRESENTING, false)] {
            for caps in pattern.captures_iter(&text) {
                let whole = caps.get(0).expect("group 0 always exists");
                let Some(name) = clean_name(&caps["name"]) else { continue };
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
                        basis: if has_basis { Some(caps["basis"].trim().to_string()) } else { None },
                    }),
                    fiscal_year,
                    quote: whole.as_str().to_string(),
                });
            }
        }

        for caps in CUSTOMER_PAIR.captures_iter(&text) {
            let whole = caps.get(0).expect("group 0 always exists");
            for (who, pct) in [("a", "p1"), ("b", "p2")] {
                let Some(name) = clean_name(&caps[who]) else { continue };
                let Ok(value) = caps[pct].parse::<f64>() else { continue };
                if value <= 0.0 || value > 100.0 {
                    continue;
                }
                out.push(RawRelation {
                    counterparty: name,
                    direction: Direction::Customer,
                    amount: Some(Amount { value, unit: Unit::Percent, basis: Some(caps["basis"].to_string()) }),
                    fiscal_year,
                    quote: whole.as_str().to_string(),
                });
            }
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
    // Two letters are accepted only as an abbreviation in capitals, such as "GM".
    let abbreviation = name.len() == 2 && name.bytes().all(|b| b.is_ascii_uppercase());
    if name.chars().count() < 3 && !abbreviation {
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

    /// Sentences as they stand in annual reports filed with the SEC in 2026.
    #[test]
    fn real_phrasings_from_filed_reports() {
        // Lear Corp: the name is followed by a description before the verb.
        let lear = run("In 2025, General Motors, one of the largest automotive and light truck manufacturers in the world, accounted for 22% of our net sales.");
        assert_eq!(lear.len(), 1, "{lear:?}");
        assert_eq!(lear[0].counterparty, "General Motors");
        assert_eq!(lear[0].amount.as_ref().unwrap().value, 22.0);
        assert_eq!(lear[0].fiscal_year, Some(2025));

        // Aspen Aerogels: the customer is written as an abbreviation.
        let aspen = run("GM represented 59% of our total revenue in 2025 and was our only customer representing 10% or more of our revenue for that period.");
        assert_eq!(aspen.len(), 1, "{aspen:?}");
        assert_eq!(aspen[0].counterparty, "GM");
        assert_eq!(aspen[0].amount.as_ref().unwrap().value, 59.0);
        assert_eq!(aspen[0].amount.as_ref().unwrap().basis.as_deref(), Some("total revenue"));
    }

    #[test]
    fn phinia_phrasings() {
        let a = run("The Company\u{2019}s worldwide net sales to General Motors Company during the years ended December 31, 2025, 2024, and 2023 were 18%, 17% , and 16%, respectively.");
        assert_eq!(a.len(), 1, "{a:?}");
        assert_eq!(a[0].counterparty, "General Motors Company");
        assert_eq!(a[0].amount.as_ref().unwrap().value, 18.0);
        let b = run("Our top five customers accounted for approximately 37% of our net sales, with General Motors Company representing 18%.");
        assert!(b.iter().any(|r| r.counterparty == "General Motors Company" && r.amount.as_ref().unwrap().value == 18.0), "{b:?}");
        let c = run("Consolidated net sales to General Motors Company (including its subsidiaries) were approximately 18 %, 17 %, and 16 % for the years ended December 31, 2025.");
        assert_eq!(c.len(), 1, "{c:?}");
    }

    #[test]
    fn skips_anonymous_and_generic_subjects() {
        assert!(run("Customer A accounted for 14% of our net sales in 2025.").is_empty());
        assert!(run("No single customer accounted for 10% of our revenues.").is_empty());
        assert!(run("We accounted for 10% of our revenues.").is_empty());
    }

    #[test]
    fn pairs_two_customers_with_two_numbers_in_order() {
        let found = run("Alpha Stores Inc. and Beta Markets Inc. accounted for 12% and 10% of net sales.");
        assert_eq!(found.len(), 2, "got {found:?}");
        assert_eq!(found[0].counterparty, "Alpha Stores Inc");
        assert_eq!(found[0].amount.as_ref().unwrap().value, 12.0);
        assert_eq!(found[1].counterparty, "Beta Markets Inc");
        assert_eq!(found[1].amount.as_ref().unwrap().value, 10.0);
        let aspen = run("GM and Distribution represented 59% and 9% of our total revenue in 2025.");
        assert_eq!(aspen.len(), 2, "got {aspen:?}");
        assert_eq!(aspen[0].counterparty, "GM");
        assert_eq!(aspen[0].amount.as_ref().unwrap().value, 59.0);
    }

    #[test]
    fn three_names_with_two_numbers_stay_unparsed() {
        let found = run("Alpha Stores Inc., Beta Markets Inc. and Gamma Retail Inc. accounted for 12% and 10% of net sales.");
        assert!(found.len() <= 2, "got {found:?}");
        assert!(found.iter().all(|r| r.counterparty != "Alpha Stores Inc"), "got {found:?}");
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
