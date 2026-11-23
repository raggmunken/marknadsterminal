//! Shared types for the terminal. This crate does no I/O.

pub mod names;
pub mod time;

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CoreError {
    #[error("invalid CIK: {0:?}")]
    InvalidCik(String),
}

/// SEC Central Index Key.
///
/// `Display` prints the zero-padded ten digit form that the EDGAR
/// submissions endpoint expects. Archive paths use [`Cik::unpadded`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct Cik(u64);

impl Cik {
    pub const MAX: u64 = 9_999_999_999;

    pub fn new(value: u64) -> Result<Self, CoreError> {
        if value == 0 || value > Self::MAX {
            return Err(CoreError::InvalidCik(value.to_string()));
        }
        Ok(Self(value))
    }

    pub fn get(self) -> u64 {
        self.0
    }

    pub fn unpadded(self) -> String {
        self.0.to_string()
    }
}

impl TryFrom<u64> for Cik {
    type Error = CoreError;
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Cik::new(value)
    }
}

impl From<Cik> for u64 {
    fn from(cik: Cik) -> u64 {
        cik.0
    }
}

impl fmt::Display for Cik {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:010}", self.0)
    }
}

impl FromStr for Cik {
    type Err = CoreError;

    /// Accepts `1467858`, `0001467858` and `CIK0001467858`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        let digits = trimmed
            .strip_prefix("CIK")
            .or_else(|| trimmed.strip_prefix("cik"))
            .unwrap_or(trimmed);
        if digits.is_empty() || digits.len() > 10 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(CoreError::InvalidCik(s.to_string()));
        }
        let value: u64 = digits
            .parse()
            .map_err(|_| CoreError::InvalidCik(s.to_string()))?;
        Cik::new(value)
    }
}

/// What the counterparty is to the subject company.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// The counterparty buys from the subject.
    Customer,
    /// The counterparty sells to the subject.
    Supplier,
}

/// How well a relation is supported.
///
/// The plan names the three levels. Their exact meaning here is the
/// prototype's reading of the plan and is open for review:
/// * `Disclosed`: the subject's own filing names the counterparty and gives an amount.
/// * `Registry`: a public register states the relation, for example a
///   federal subcontract. Hard evidence, but not the company's own words.
/// * `Named`: the subject's own filing names the counterparty without an amount.
/// * `Reverse`: the relation was found in the counterparty's filing (step 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Disclosed,
    Registry,
    Named,
    Reverse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Unit {
    Percent,
    Usd,
}

/// A reported size of a relation, for example 21 percent of net sales.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Amount {
    pub value: f64,
    pub unit: Unit,
    /// What the value is measured against, as written in the source ("net sales").
    pub basis: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Period {
    pub fiscal_year: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counterparty {
    /// Name as written in the source.
    pub name: String,
    /// Filled in by entity resolution (step 5). `None` until then.
    pub cik: Option<Cik>,
}

/// Where a relation comes from. The quote must occur in the source text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub accession: Option<String>,
    pub url: Option<String>,
    pub quote: String,
}

/// One edge in the supply chain graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Relation {
    pub subject: Cik,
    pub counterparty: Counterparty,
    pub direction: Direction,
    pub amount: Option<Amount>,
    pub period: Option<Period>,
    pub evidence: Evidence,
    pub confidence: Confidence,
    /// True when both companies' own filings state the relation (step 7).
    #[serde(default)]
    pub corroborated: bool,
}

/// A company in the directory used for entity resolution (step 5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Company {
    pub cik: Cik,
    pub name: String,
    pub ticker: Option<String>,
}

/// Step 8. Nothing reaches a signal before a person has approved it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewStatus {
    Pending,
    Approved,
    Rejected,
}

/// A relation as it sits in the store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredRelation {
    pub id: i64,
    #[serde(flatten)]
    pub relation: Relation,
    pub status: ReviewStatus,
    pub note: Option<String>,
    /// Unix seconds when the store first learned of the current content.
    /// Historical tests may only read rows with `known_at` before the test date.
    pub known_at: i64,
    pub reviewed_at: Option<i64>,
}

/// One row of the append only signal log. `hash` covers `prev_hash`,
/// `created_at` and `payload`, so a changed row breaks every later row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogRecord {
    pub seq: u64,
    pub created_at: i64,
    pub payload: String,
    pub prev_hash: String,
    pub hash: String,
}

/// Something that happened in the world and may disturb a supply chain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Where it was fetched from, for example `usgs`.
    pub source: String,
    /// The source's own identifier. Unique together with `source`.
    pub external_id: String,
    /// `earthquake`, `cyclone`, `flood`, `volcano`, `drought`, `wildfire` or the source's own code.
    pub kind: String,
    pub title: String,
    /// Unix seconds, UTC.
    pub time: i64,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub severity: Option<f64>,
    pub severity_unit: Option<String>,
    /// The source's alert level, for example `Red`.
    pub alert: Option<String>,
    pub country: Option<String>,
    pub url: Option<String>,
}

/// A place tied to a company: a plant, a warehouse, a head office.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Location {
    /// The company the place belongs to, as a name.
    pub owner: String,
    pub label: String,
    pub lat: f64,
    pub lon: f64,
    /// `manual` or the register it came from, for example `epa-frs`.
    pub source: String,
    /// The register's own identifier. Unique together with `source`.
    pub external_id: String,
}

/// One observation in a time series. `date` is as the source gives it:
/// a day, or a year for annual series.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeriesPoint {
    pub date: String,
    pub value: f64,
}

/// One name on a sanctions list. Free text remarks are deliberately not
/// kept, because they hold personal details of private individuals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SanctionEntry {
    pub list_id: u64,
    pub name: String,
    /// `individual`, `vessel`, `aircraft`, or `None` for organisations.
    pub kind: Option<String>,
    pub program: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cik_parses_all_three_spellings() {
        for s in ["1467858", "0001467858", "CIK0001467858", " 1467858 "] {
            assert_eq!(s.parse::<Cik>().unwrap().get(), 1_467_858, "input {s:?}");
        }
    }

    #[test]
    fn cik_prints_padded_and_unpadded() {
        let cik = Cik::new(1_467_858).unwrap();
        assert_eq!(cik.to_string(), "0001467858");
        assert_eq!(cik.unpadded(), "1467858");
    }

    #[test]
    fn cik_rejects_garbage() {
        for s in ["", "0", "abc", "12a", "12345678901", "-5", "CIK"] {
            assert!(s.parse::<Cik>().is_err(), "input {s:?} should fail");
        }
    }

    #[test]
    fn cik_json_is_a_number_and_is_validated() {
        let cik: Cik = serde_json::from_str("1467858").unwrap();
        assert_eq!(serde_json::to_string(&cik).unwrap(), "1467858");
        assert!(serde_json::from_str::<Cik>("0").is_err());
    }

    #[test]
    fn relation_round_trips_through_json() {
        let relation = Relation {
            subject: Cik::new(1).unwrap(),
            counterparty: Counterparty { name: "Example Corp".into(), cik: None },
            direction: Direction::Customer,
            amount: Some(Amount { value: 21.0, unit: Unit::Percent, basis: Some("net sales".into()) }),
            period: Some(Period { fiscal_year: 2025 }),
            evidence: Evidence { accession: None, url: None, quote: "Example Corp accounted for 21% of net sales".into() },
            confidence: Confidence::Disclosed,
            corroborated: false,
        };
        let json = serde_json::to_string(&relation).unwrap();
        assert_eq!(serde_json::from_str::<Relation>(&json).unwrap(), relation);
        assert!(json.contains("\"direction\":\"customer\""));
    }
}
