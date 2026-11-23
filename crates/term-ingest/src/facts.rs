//! SEC "company facts": every number a company has tagged in its filings.
//! This is what fills the financials view. No key, the same contact
//! address as the rest of EDGAR.
//!
//! STATUS: run live 2026-11-23 for General Motors and tested against a
//! recorded real answer, cut down to three tags.

use crate::{EdgarClient, IngestError};
use serde_json::Value;
use std::collections::BTreeMap;
use term_core::{Cik, SeriesPoint};

pub fn url(cik: Cik) -> String {
    format!("https://data.sec.gov/api/xbrl/companyfacts/CIK{cik}.json")
}

/// The lines shown in the financials view, with the tags that can carry
/// them. Companies choose between several tags for revenue, so the first
/// tag that has annual values wins.
pub const LINES: &[(&str, &[&str])] = &[
    ("Intäkter", &["Revenues", "RevenueFromContractWithCustomerExcludingAssessedTax", "SalesRevenueNet"]),
    ("Nettoresultat", &["NetIncomeLoss"]),
    ("Tillgångar", &["Assets"]),
    ("Skulder", &["Liabilities"]),
    ("Eget kapital", &["StockholdersEquity"]),
];

/// Annual values in US dollars for one tag, oldest first, one per year end.
///
/// An annual report repeats earlier years and sometimes holds a quarter
/// with the same end date. For each end date the longest period wins, and
/// among equals the value filed last.
pub fn annual(json: &str, tag: &str) -> Result<Vec<SeriesPoint>, IngestError> {
    let root: Value = serde_json::from_str(json).map_err(|e| IngestError::Format(e.to_string()))?;
    let facts = root
        .get("facts")
        .and_then(|f| f.get("us-gaap"))
        .ok_or_else(|| IngestError::Format("no us-gaap facts in the answer".into()))?;
    let Some(rows) = facts.get(tag).and_then(|t| t.get("units")).and_then(|u| u.get("USD")).and_then(Value::as_array)
    else {
        return Ok(Vec::new());
    };

    // end date -> (start date, filed date, value)
    let mut best: BTreeMap<String, (String, String, f64)> = BTreeMap::new();
    for row in rows {
        let text = |key: &str| row.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
        if text("form") != "10-K" || text("fp") != "FY" {
            continue;
        }
        let (Some(value), end) = (row.get("val").and_then(Value::as_f64), text("end")) else { continue };
        if end.is_empty() {
            continue;
        }
        let (start, filed) = (text("start"), text("filed"));
        // A duration under about 300 days is a quarter, not a year. Balance
        // sheet items have no start and are always kept.
        if !start.is_empty() {
            let days = term_core::time::iso_to_unix(&end)
                .zip(term_core::time::iso_to_unix(&start))
                .map(|(e, s)| (e - s) / 86_400);
            if days.is_some_and(|d| d < 300) {
                continue;
            }
        }
        let better = match best.get(&end) {
            None => true,
            // An earlier start is a longer period. Empty sorts first, which is right for balances.
            Some((old_start, old_filed, _)) => (&start, std::cmp::Reverse(&filed)) < (old_start, std::cmp::Reverse(old_filed)),
        };
        if better {
            best.insert(end, (start, filed, value));
        }
    }
    Ok(best.into_iter().map(|(date, (_, _, value))| SeriesPoint { date, value }).collect())
}

/// One line of the statement: label, the tag it was read from, and the values.
pub type Line = (&'static str, &'static str, Vec<SeriesPoint>);

/// Every line in [`LINES`] that the company reports.
pub fn statement(json: &str) -> Result<Vec<Line>, IngestError> {
    let mut out = Vec::new();
    for (label, tags) in LINES {
        for tag in *tags {
            let points = annual(json, tag)?;
            if !points.is_empty() {
                out.push((*label, *tag, points));
                break;
            }
        }
    }
    Ok(out)
}

impl EdgarClient {
    pub async fn company_facts(&self, cik: Cik) -> Result<String, IngestError> {
        self.get_text(&url(cik)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recorded() -> String {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/kallor/sec_companyfacts_gm.json");
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn reads_recorded_annual_values() {
        let revenue = annual(&recorded(), "Revenues").unwrap();
        let last = revenue.last().unwrap();
        assert_eq!(last.date, "2025-12-31");
        assert_eq!(last.value, 185_019_000_000.0);
        assert!(revenue.windows(2).all(|w| w[0].date < w[1].date), "one value per year end, in order");

        let assets = annual(&recorded(), "Assets").unwrap();
        assert_eq!(assets.last().unwrap().value, 281_284_000_000.0);
    }

    #[test]
    fn statement_uses_the_first_tag_that_has_values() {
        let lines = statement(&recorded()).unwrap();
        let labels: Vec<&str> = lines.iter().map(|l| l.0).collect();
        assert_eq!(labels, ["Intäkter", "Nettoresultat", "Tillgångar"], "the recording holds three tags");
        assert_eq!(lines[0].1, "Revenues");
    }

    #[test]
    fn quarters_and_other_forms_are_left_out_and_the_longest_period_wins() {
        let json = r#"{"facts": {"us-gaap": {"Revenues": {"units": {"USD": [
            {"start": "2025-10-01", "end": "2025-12-31", "val": 40, "fp": "FY", "form": "10-K", "filed": "2026-01-27"},
            {"start": "2025-01-01", "end": "2025-12-31", "val": 185, "fp": "FY", "form": "10-K", "filed": "2026-01-27"},
            {"start": "2025-01-01", "end": "2025-12-31", "val": 186, "fp": "FY", "form": "10-K", "filed": "2027-01-27"},
            {"start": "2025-01-01", "end": "2025-06-30", "val": 90, "fp": "Q2", "form": "10-Q", "filed": "2025-07-25"}
        ]}}}}}"#;
        assert_eq!(annual(json, "Revenues").unwrap(), vec![SeriesPoint { date: "2025-12-31".into(), value: 186.0 }]);
        assert!(annual(json, "Assets").unwrap().is_empty(), "a missing tag is not an error");
        assert!(annual("{}", "Assets").is_err());
    }

    #[test]
    fn address_uses_the_padded_cik() {
        assert_eq!(url(Cik::new(1_467_858).unwrap()), "https://data.sec.gov/api/xbrl/companyfacts/CIK0001467858.json");
    }
}
