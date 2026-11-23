//! USAspending: federal subcontracts. No key.
//!
//! A subaward row says that a prime contractor paid a subcontractor under
//! a federal contract. Searching for a company returns the rows where it
//! is the subcontractor, so each prime contractor found is a customer of
//! the company. This is register data, not the company's own words, and
//! it only covers work under US federal contracts.

use crate::{format_err, Http, Result, SourceError};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use term_core::names::normalize_name;
use term_core::{Amount, Cik, Confidence, Counterparty, Direction, Evidence, Period, Relation, Unit};

pub const URL: &str = "https://api.usaspending.gov/api/v2/search/spending_by_award/";

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Subaward {
    #[serde(rename = "Sub-Award ID")]
    pub id: String,
    #[serde(rename = "Sub-Awardee Name")]
    pub subcontractor: String,
    #[serde(rename = "Prime Recipient Name")]
    pub prime: String,
    #[serde(rename = "Sub-Award Date")]
    pub date: String,
    #[serde(rename = "Sub-Award Amount")]
    pub amount: f64,
    #[serde(rename = "Prime Award ID")]
    pub prime_award_id: Option<String>,
    #[serde(rename = "prime_award_generated_internal_id")]
    pub prime_award_key: Option<String>,
}

/// The largest subawards to `company` between two dates (YYYY-MM-DD).
pub fn request_body(company: &str, from: &str, to: &str, limit: u32) -> Result<Value> {
    let company = company.trim();
    if company.chars().count() < 3 {
        return Err(SourceError::Config("the company name must have at least three characters".into()));
    }
    Ok(json!({
        "subawards": true,
        "limit": limit.clamp(1, 100),
        "page": 1,
        "filters": {
            "award_type_codes": ["A", "B", "C", "D"],
            "recipient_search_text": [company.to_uppercase()],
            "time_period": [{"start_date": from, "end_date": to}]
        },
        "fields": ["Sub-Award ID", "Sub-Awardee Name", "Sub-Award Date", "Sub-Award Amount",
                   "Prime Recipient Name", "Prime Award ID"],
        "sort": "Sub-Award Amount",
        "order": "desc"
    }))
}

#[derive(Deserialize)]
struct Envelope {
    results: Vec<Subaward>,
}

pub fn parse(json: &str) -> Result<Vec<Subaward>> {
    let envelope: Envelope = serde_json::from_str(json).map_err(format_err)?;
    Ok(envelope.results)
}

pub async fn fetch(http: &Http, company: &str, from: &str, to: &str, limit: u32) -> Result<Vec<Subaward>> {
    parse(&http.post_json(URL, &request_body(company, from, to, limit)?, &[]).await?)
}

/// One customer relation per prime contractor, with the subawards summed.
///
/// Only rows whose subcontractor name equals `company_name` after
/// normalisation are used, because the search also returns companies that
/// merely contain the search words. The evidence line is generated from
/// the register row and names the largest subaward.
pub fn customer_relations(subject: Cik, company_name: &str, rows: &[Subaward]) -> Vec<Relation> {
    let wanted = normalize_name(company_name);
    let mut by_prime: BTreeMap<String, Vec<&Subaward>> = BTreeMap::new();
    for row in rows {
        if normalize_name(&row.subcontractor) == wanted && row.amount > 0.0 && normalize_name(&row.prime) != wanted {
            by_prime.entry(normalize_name(&row.prime)).or_default().push(row);
        }
    }
    by_prime
        .into_values()
        .map(|mut group| {
            group.sort_by(|a, b| b.amount.total_cmp(&a.amount));
            let largest = group[0];
            let total: f64 = group.iter().map(|r| r.amount).sum();
            let year = group.iter().filter_map(|r| r.date.get(..4)?.parse::<u16>().ok()).max();
            Relation {
                subject,
                counterparty: Counterparty { name: largest.prime.clone(), cik: None },
                direction: Direction::Customer,
                amount: Some(Amount { value: total, unit: Unit::Usd, basis: Some("federal subawards".into()) }),
                period: year.map(|fiscal_year| Period { fiscal_year }),
                evidence: Evidence {
                    accession: None,
                    url: largest
                        .prime_award_key
                        .as_ref()
                        .map(|key| format!("https://www.usaspending.gov/award/{key}")),
                    quote: format!(
                        "USAspending: {} subaward(s) from {} to {}. Largest: {} on {}, {:.0} USD, prime award {}.",
                        group.len(),
                        largest.prime,
                        largest.subcontractor,
                        largest.id,
                        largest.date,
                        largest.amount,
                        largest.prime_award_id.as_deref().unwrap_or("unknown")
                    ),
                },
                confidence: Confidence::Registry,
                corroborated: false,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_recorded_subawards() {
        let rows = parse(&crate::fixture("usaspending_subawards.json")).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].subcontractor, "LOCKHEED MARTIN CORPORATION");
        assert_eq!(rows[0].prime, "RAYTHEON COMPANY");
        assert_eq!(rows[0].amount, 168_638_833.0);
        assert_eq!(rows[0].date, "2025-10-07");
        assert_eq!(rows[0].prime_award_id.as_deref(), Some("FA861124CB001"));
    }

    #[test]
    fn primes_become_customers_with_summed_amounts() {
        let rows = parse(&crate::fixture("usaspending_subawards.json")).unwrap();
        let relations = customer_relations(Cik::new(936_468).unwrap(), "Lockheed Martin Corp", &rows);
        assert_eq!(relations.len(), 2, "two primes in three rows");
        let northrop = relations.iter().find(|r| r.counterparty.name.starts_with("NORTHROP")).unwrap();
        assert_eq!(northrop.direction, Direction::Customer);
        assert_eq!(northrop.confidence, Confidence::Registry);
        let amount = northrop.amount.as_ref().unwrap();
        assert_eq!(amount.unit, Unit::Usd);
        assert!((amount.value - (145_148_167.0 + 72_342_317.42)).abs() < 0.01);
        assert_eq!(northrop.period, Some(Period { fiscal_year: 2026 }));
        assert!(northrop.evidence.quote.contains("2 subaward(s)") && northrop.evidence.quote.contains("5300174687"));
        assert!(northrop.evidence.url.as_deref().unwrap().starts_with("https://www.usaspending.gov/award/CONT_AWD_"));
    }

    #[test]
    fn rows_for_other_companies_are_ignored() {
        let rows = parse(&crate::fixture("usaspending_subawards.json")).unwrap();
        assert!(customer_relations(Cik::new(1).unwrap(), "Lockheed Martin Aeronautics", &rows).is_empty());
    }

    #[test]
    fn body_asks_for_subawards() {
        let body = request_body("Lockheed Martin", "2025-10-01", "2026-09-30", 500).unwrap();
        assert_eq!(body["subawards"], true);
        assert_eq!(body["limit"], 100);
        assert_eq!(body["filters"]["recipient_search_text"][0], "LOCKHEED MARTIN");
        assert!(request_body("LM", "2025-10-01", "2026-09-30", 5).is_err());
    }
}
