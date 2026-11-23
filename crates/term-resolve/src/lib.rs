//! Chain engine steps 5 to 7: match names to companies, turn what the
//! other side says into a relation, and merge everything into one edge
//! per pair.
//!
//! STATUS: unit tested on made up companies. The reader for SEC's ticker
//! file follows the published shape of `company_tickers.json` but has not
//! been run on the real file.

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use term_core::names::normalize_name;
use term_core::{Cik, Company, Confidence, Counterparty, Direction, Relation};

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("unexpected directory format: {0}")]
    Format(String),
}

/// Fuzzy matches below this token overlap are not reported at all.
pub const MIN_FUZZY_SCORE: f64 = 0.6;

/// Outcome of looking a name up in the directory.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    /// The normalised name matches exactly one company.
    Exact(Cik),
    /// The best partial match. Never applied automatically. A person decides.
    Fuzzy { cik: Cik, score: f64 },
    /// Several companies fit equally well.
    Ambiguous(Vec<Cik>),
    Unknown,
}

/// The list of known companies.
#[derive(Debug, Default)]
pub struct Directory {
    companies: Vec<Company>,
    by_name: HashMap<String, Vec<usize>>,
    tokens: Vec<BTreeSet<String>>,
}

#[derive(Deserialize)]
struct SecTickerRow {
    cik_str: u64,
    ticker: String,
    title: String,
}

impl Directory {
    /// Builds the directory. A CIK that occurs twice keeps its first row,
    /// which is how the SEC file lists several share classes of one company.
    pub fn new(companies: impl IntoIterator<Item = Company>) -> Self {
        let mut dir = Directory::default();
        let mut seen = BTreeSet::new();
        for company in companies {
            if !seen.insert(company.cik) {
                continue;
            }
            let norm = normalize_name(&company.name);
            dir.by_name.entry(norm.clone()).or_default().push(dir.companies.len());
            dir.tokens.push(norm.split(' ').map(str::to_string).collect());
            dir.companies.push(company);
        }
        dir
    }

    /// Reads SEC's `company_tickers.json`: an object whose values look like
    /// `{"cik_str": 320193, "ticker": "AAPL", "title": "Apple Inc."}`.
    pub fn from_sec_tickers_json(json: &str) -> Result<Self, ResolveError> {
        let rows: BTreeMap<String, SecTickerRow> =
            serde_json::from_str(json).map_err(|e| ResolveError::Format(e.to_string()))?;
        // The keys are row numbers as strings. Sort numerically so that the
        // first listed share class wins.
        let mut ordered: Vec<(u64, SecTickerRow)> = rows
            .into_iter()
            .map(|(k, v)| (k.parse::<u64>().unwrap_or(u64::MAX), v))
            .collect();
        ordered.sort_by_key(|(k, _)| *k);
        let mut companies = Vec::with_capacity(ordered.len());
        for (_, row) in ordered {
            let cik = Cik::new(row.cik_str).map_err(|e| ResolveError::Format(e.to_string()))?;
            companies.push(Company { cik, name: row.title, ticker: Some(row.ticker) });
        }
        Ok(Self::new(companies))
    }

    pub fn companies(&self) -> &[Company] {
        &self.companies
    }

    pub fn len(&self) -> usize {
        self.companies.len()
    }

    pub fn is_empty(&self) -> bool {
        self.companies.is_empty()
    }

    pub fn resolve(&self, name: &str) -> Resolution {
        let norm = normalize_name(name);
        if norm.is_empty() {
            return Resolution::Unknown;
        }
        if let Some(hits) = self.by_name.get(&norm) {
            return match hits.as_slice() {
                [one] => Resolution::Exact(self.companies[*one].cik),
                many => Resolution::Ambiguous(many.iter().map(|i| self.companies[*i].cik).collect()),
            };
        }

        let query: BTreeSet<String> = norm.split(' ').map(str::to_string).collect();
        let mut best_score = 0.0_f64;
        let mut best: Vec<usize> = Vec::new();
        for (i, tokens) in self.tokens.iter().enumerate() {
            let shared = query.intersection(tokens).count();
            if shared == 0 {
                continue;
            }
            let score = shared as f64 / query.union(tokens).count() as f64;
            if score > best_score + 1e-9 {
                best_score = score;
                best = vec![i];
            } else if (score - best_score).abs() <= 1e-9 {
                best.push(i);
            }
        }
        if best_score < MIN_FUZZY_SCORE {
            return Resolution::Unknown;
        }
        match best.as_slice() {
            [one] => Resolution::Fuzzy { cik: self.companies[*one].cik, score: best_score },
            many => Resolution::Ambiguous(many.iter().map(|i| self.companies[*i].cik).collect()),
        }
    }

    /// Step 5 for one relation. Only an exact match fills in the CIK.
    pub fn apply(&self, relation: &mut Relation) -> Resolution {
        let resolution = self.resolve(&relation.counterparty.name);
        if let Resolution::Exact(cik) = resolution {
            // A company cannot be its own customer or supplier.
            if cik != relation.subject {
                relation.counterparty.cik = Some(cik);
            }
        }
        resolution
    }
}

/// What B's filing says about A, seen from A's side.
///
/// If B reports "A is our customer", then B is a supplier of A. The amount
/// is dropped because it is measured against B's sales, not A's. The
/// evidence still points at B's filing. Returns `None` until the
/// counterparty has been resolved to a CIK.
pub fn mirror(relation: &Relation, subject_name: &str) -> Option<Relation> {
    let other = relation.counterparty.cik?;
    Some(Relation {
        subject: other,
        counterparty: Counterparty { name: subject_name.to_string(), cik: Some(relation.subject) },
        direction: match relation.direction {
            Direction::Customer => Direction::Supplier,
            Direction::Supplier => Direction::Customer,
        },
        amount: None,
        period: relation.period,
        evidence: relation.evidence.clone(),
        confidence: Confidence::Reverse,
        corroborated: false,
    })
}

fn rank(confidence: Confidence) -> u8 {
    match confidence {
        Confidence::Disclosed => 0,
        Confidence::Registry => 1,
        Confidence::Named => 2,
        Confidence::Reverse => 3,
    }
}

fn counterparty_key(c: &Counterparty) -> String {
    match c.cik {
        Some(cik) => format!("cik:{cik}"),
        None => format!("name:{}", normalize_name(&c.name)),
    }
}

/// Step 7. One relation per subject, counterparty and direction.
///
/// The best supported relation wins: the subject's own statement with an
/// amount, then its own statement without, then the other side's
/// statement. When the group holds both an own statement and a reverse
/// one, the result is marked as corroborated.
pub fn merge(relations: Vec<Relation>) -> Vec<Relation> {
    let mut groups: BTreeMap<(Cik, String, bool), Vec<Relation>> = BTreeMap::new();
    for r in relations {
        let key = (r.subject, counterparty_key(&r.counterparty), r.direction == Direction::Customer);
        groups.entry(key).or_default().push(r);
    }
    groups
        .into_values()
        .map(|mut group| {
            let own = group.iter().any(|r| r.confidence != Confidence::Reverse);
            let reverse = group.iter().any(|r| r.confidence == Confidence::Reverse);
            group.sort_by_key(|r| {
                (
                    rank(r.confidence),
                    r.amount.is_none(),
                    std::cmp::Reverse(r.period.map(|p| p.fiscal_year).unwrap_or(0)),
                )
            });
            let mut best = group.swap_remove(0);
            best.corroborated = own && reverse;
            best
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use term_core::{Amount, Evidence, Period, Unit};

    fn cik(n: u64) -> Cik {
        Cik::new(n).unwrap()
    }

    fn company(n: u64, name: &str) -> Company {
        Company { cik: cik(n), name: name.into(), ticker: None }
    }

    fn directory() -> Directory {
        Directory::new([
            company(10, "Haldenfjord Motors Corp."),
            company(11, "Haldenfjord Motors Financial Inc."),
            company(20, "Polarvik Castings Ltd"),
            company(30, "Twin Name Inc."),
            company(31, "Twin Name Corp."),
        ])
    }

    fn relation(subject: u64, name: &str, direction: Direction, confidence: Confidence) -> Relation {
        Relation {
            subject: cik(subject),
            counterparty: Counterparty { name: name.into(), cik: None },
            direction,
            amount: None,
            period: None,
            evidence: Evidence { accession: Some(format!("acc-{subject}")), url: None, quote: "q".into() },
            confidence,
            corroborated: false,
        }
    }

    #[test]
    fn exact_match_ignores_suffix_and_case() {
        let dir = directory();
        assert_eq!(dir.resolve("HALDENFJORD MOTORS CORPORATION"), Resolution::Exact(cik(10)));
        assert_eq!(dir.resolve("Polarvik Castings Ltd."), Resolution::Exact(cik(20)));
    }

    #[test]
    fn same_name_twice_is_ambiguous() {
        assert_eq!(directory().resolve("Twin Name"), Resolution::Ambiguous(vec![cik(30), cik(31)]));
    }

    #[test]
    fn partial_overlap_is_fuzzy_and_never_exact() {
        let dir = directory();
        match dir.resolve("Polarvik Castings Group") {
            Resolution::Fuzzy { cik: c, score } => {
                assert_eq!(c, cik(20));
                assert!((score - 2.0 / 3.0).abs() < 1e-9);
            }
            other => panic!("expected fuzzy, got {other:?}"),
        }
        assert_eq!(dir.resolve("Completely Different Name"), Resolution::Unknown);
        assert_eq!(dir.resolve(""), Resolution::Unknown);
    }

    #[test]
    fn apply_sets_cik_only_on_exact_match() {
        let dir = directory();
        let mut exact = relation(1, "Haldenfjord Motors Corp", Direction::Customer, Confidence::Named);
        dir.apply(&mut exact);
        assert_eq!(exact.counterparty.cik, Some(cik(10)));

        let mut fuzzy = relation(1, "Polarvik Castings Group", Direction::Supplier, Confidence::Named);
        assert!(matches!(dir.apply(&mut fuzzy), Resolution::Fuzzy { .. }));
        assert_eq!(fuzzy.counterparty.cik, None);

        let mut own = relation(10, "Haldenfjord Motors Corp", Direction::Customer, Confidence::Named);
        dir.apply(&mut own);
        assert_eq!(own.counterparty.cik, None, "a company is not its own customer");
    }

    #[test]
    fn duplicate_cik_keeps_first_row() {
        let dir = Directory::new([company(1, "First Name Inc."), company(1, "Second Class Shares")]);
        assert_eq!(dir.len(), 1);
        assert_eq!(dir.resolve("First Name"), Resolution::Exact(cik(1)));
        assert_eq!(dir.resolve("Second Class Shares"), Resolution::Unknown);
    }

    #[test]
    fn reads_sec_ticker_shape() {
        let json = r#"{
            "0": {"cik_str": 100, "ticker": "AAA", "title": "Alpha Works Inc."},
            "1": {"cik_str": 200, "ticker": "BBB", "title": "Beta Forge Corp"},
            "2": {"cik_str": 100, "ticker": "AAA.B", "title": "Alpha Works Inc."}
        }"#;
        let dir = Directory::from_sec_tickers_json(json).unwrap();
        assert_eq!(dir.len(), 2);
        assert_eq!(dir.companies()[0].ticker.as_deref(), Some("AAA"));
        assert_eq!(dir.resolve("Beta Forge"), Resolution::Exact(cik(200)));
        assert!(Directory::from_sec_tickers_json("[]").is_err());
    }

    #[test]
    fn mirror_flips_direction_and_drops_amount() {
        let mut r = relation(20, "Nordvik Test Components", Direction::Customer, Confidence::Disclosed);
        r.amount = Some(Amount { value: 30.0, unit: Unit::Percent, basis: None });
        assert!(mirror(&r, "Polarvik Castings Ltd").is_none(), "unresolved counterparty cannot be mirrored");

        r.counterparty.cik = Some(cik(42));
        let m = mirror(&r, "Polarvik Castings Ltd").unwrap();
        assert_eq!(m.subject, cik(42));
        assert_eq!(m.counterparty.cik, Some(cik(20)));
        assert_eq!(m.counterparty.name, "Polarvik Castings Ltd");
        assert_eq!(m.direction, Direction::Supplier);
        assert_eq!(m.confidence, Confidence::Reverse);
        assert!(m.amount.is_none());
        assert_eq!(m.evidence, r.evidence);
    }

    #[test]
    fn merge_prefers_own_statement_and_marks_corroboration() {
        let mut own = relation(42, "Polarvik Castings Ltd", Direction::Supplier, Confidence::Named);
        own.counterparty.cik = Some(cik(20));
        let mut theirs = relation(20, "Nordvik", Direction::Customer, Confidence::Disclosed);
        theirs.counterparty.cik = Some(cik(42));
        let reverse = mirror(&theirs, "Polarvik Castings Ltd").unwrap();

        let merged = merge(vec![reverse, own, theirs]);
        assert_eq!(merged.len(), 2);
        let edge = merged.iter().find(|r| r.subject == cik(42)).unwrap();
        assert_eq!(edge.confidence, Confidence::Named);
        assert!(edge.corroborated);
        assert_eq!(edge.evidence.accession.as_deref(), Some("acc-42"));
        let other = merged.iter().find(|r| r.subject == cik(20)).unwrap();
        assert!(!other.corroborated, "only one side has spoken about this edge");
    }

    #[test]
    fn merge_prefers_amount_then_newest_year() {
        let mut a = relation(1, "Acme Parts", Direction::Customer, Confidence::Disclosed);
        a.amount = Some(Amount { value: 10.0, unit: Unit::Percent, basis: None });
        a.period = Some(Period { fiscal_year: 2023 });
        let mut b = a.clone();
        b.amount = Some(Amount { value: 12.0, unit: Unit::Percent, basis: None });
        b.period = Some(Period { fiscal_year: 2025 });
        let c = relation(1, "ACME PARTS INC", Direction::Customer, Confidence::Named);

        let merged = merge(vec![c, a, b]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].amount.as_ref().unwrap().value, 12.0);
        assert!(!merged[0].corroborated);
    }

    #[test]
    fn merge_keeps_customer_and_supplier_apart() {
        let merged = merge(vec![
            relation(1, "Acme Parts", Direction::Customer, Confidence::Named),
            relation(1, "Acme Parts", Direction::Supplier, Confidence::Named),
        ]);
        assert_eq!(merged.len(), 2);
    }
}
