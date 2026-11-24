//! Which of a company'"'"'s documents to read.
//!
//! Annual reports are not the only filings that name customers and suppliers.
//! The table below lists the document families at SEC that are free to fetch,
//! why each one matters, how many of the newest to read and whether the
//! exhibits (attached contracts and press releases) are worth reading too.
//! `docs/rapporter.md` explains the legal basis for each one.

use crate::{document_url, EdgarClient, Filing, IngestError};
use regex::Regex;
use serde::Deserialize;
use std::sync::LazyLock;
use term_core::Cik;

#[derive(Debug, Clone, Copy)]
pub struct Family {
    pub form: &'static str,
    /// How many of the newest filings of this form are read.
    pub newest: usize,
    /// Whether exhibits 10 (contracts) and 99 (press releases) are read too.
    pub exhibits: bool,
    pub why: &'static str,
}

pub const FAMILIES: &[Family] = &[
    Family { form: "10-K", newest: 1, exhibits: true, why: "annual report: customers over 10 percent, sole sources, raw materials, attached supply contracts" },
    Family { form: "20-F", newest: 1, exhibits: true, why: "annual report of a foreign filer" },
    Family { form: "40-F", newest: 1, exhibits: true, why: "annual report of a Canadian filer" },
    Family { form: "10-Q", newest: 1, exhibits: false, why: "quarterly report: concentration notes, new agreements" },
    Family { form: "8-K", newest: 8, exhibits: true, why: "material agreements (item 1.01) and press releases about contracts and awards" },
    Family { form: "6-K", newest: 4, exhibits: true, why: "foreign filer press releases and interim reports" },
    Family { form: "SD", newest: 1, exhibits: true, why: "conflict minerals report: smelters and suppliers in the chain" },
    Family { form: "S-1", newest: 1, exhibits: true, why: "IPO prospectus: customer concentration and supply agreements in detail" },
    Family { form: "424B4", newest: 1, exhibits: false, why: "final IPO prospectus" },
    Family { form: "424B5", newest: 1, exhibits: false, why: "prospectus supplement for a later offering" },
    Family { form: "DEF 14A", newest: 1, exhibits: false, why: "proxy statement: related party transactions, including supply deals" },
];

pub fn default_forms() -> Vec<&'static str> {
    FAMILIES.iter().map(|f| f.form).collect()
}

/// The filings to read: for each wanted form the newest ones, but none filed before `since`
/// (an ISO date). Newest first.
pub fn select<'a>(filings: &'a [Filing], forms: &[String], since: &str) -> Vec<(&'a Filing, Family)> {
    let mut out = Vec::new();
    for family in FAMILIES.iter().filter(|f| forms.iter().any(|w| w.eq_ignore_ascii_case(f.form))) {
        let mut of_form: Vec<&Filing> = filings
            .iter()
            .filter(|f| f.form == family.form && !f.primary_document.is_empty() && f.filing_date.as_str() >= since)
            .collect();
        of_form.sort_by(|a, b| b.filing_date.cmp(&a.filing_date));
        out.extend(of_form.into_iter().take(family.newest).map(|f| (f, *family)));
    }
    out.sort_by(|a, b| b.0.filing_date.cmp(&a.0.filing_date));
    out
}

#[derive(Deserialize)]
struct IndexJson {
    directory: Directory,
}

#[derive(Deserialize)]
struct Directory {
    #[serde(default)]
    item: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    name: String,
}

static EXHIBIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^[^/]*ex[-_]?(?:10|99)[-_.\d]*[^/]*\.htm$").expect("exhibit pattern compiles")
});

/// Names of exhibit 10 and 99 files in a filing, from its `index.json`. File names
/// are not standardised, so this is a heuristic that looks for "ex10" and "ex99".
pub fn exhibit_names(index_json: &str) -> Result<Vec<String>, IngestError> {
    let parsed: IndexJson = serde_json::from_str(index_json).map_err(|e| IngestError::Format(e.to_string()))?;
    Ok(parsed.directory.item.into_iter().map(|i| i.name).filter(|n| EXHIBIT.is_match(n)).collect())
}

pub fn index_url(cik: Cik, filing: &Filing) -> String {
    format!(
        "https://www.sec.gov/Archives/edgar/data/{}/{}/index.json",
        cik.unpadded(),
        filing.accession.replace('-', "")
    )
}

pub fn file_url(cik: Cik, filing: &Filing, name: &str) -> String {
    format!(
        "https://www.sec.gov/Archives/edgar/data/{}/{}/{}",
        cik.unpadded(),
        filing.accession.replace('-', ""),
        name
    )
}

impl EdgarClient {
    /// The documents of one filing: the main document, then at most `max_exhibits` exhibits.
    pub async fn filing_documents(&self, cik: Cik, filing: &Filing, with_exhibits: bool, max_exhibits: usize) -> Vec<String> {
        let mut urls = vec![document_url(cik, filing)];
        if with_exhibits {
            if let Ok(index) = self.get_text(&index_url(cik, filing)).await {
                if let Ok(names) = exhibit_names(&index) {
                    urls.extend(
                        names
                            .into_iter()
                            .filter(|n| *n != filing.primary_document)
                            .take(max_exhibits)
                            .map(|n| file_url(cik, filing, &n)),
                    );
                }
            }
        }
        urls
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filing(form: &str, date: &str, acc: &str) -> Filing {
        Filing { accession: acc.into(), form: form.into(), filing_date: date.into(), primary_document: "d.htm".into() }
    }

    #[test]
    fn picks_newest_per_form_and_respects_since() {
        let filings = vec![
            filing("10-K", "2026-02-01", "a-1"),
            filing("10-K", "2025-02-01", "a-2"),
            filing("8-K", "2026-05-01", "a-3"),
            filing("8-K", "2026-04-01", "a-4"),
            filing("8-K", "2024-01-01", "a-5"),
            filing("4", "2026-06-01", "a-6"),
        ];
        let forms = vec!["10-K".to_string(), "8-K".to_string(), "4".to_string()];
        let picked = select(&filings, &forms, "2025-01-01");
        let accs: Vec<&str> = picked.iter().map(|(f, _)| f.accession.as_str()).collect();
        // Newest first; only one 10-K; the 2024 8-K is too old; form 4 is not a family.
        assert_eq!(accs, vec!["a-3", "a-4", "a-1"]);
    }

    #[test]
    fn finds_exhibit_files_by_name() {
        let json = r#"{"directory":{"item":[{"name":"gm-20260101.htm"},{"name":"ex10-1.htm"},{"name":"a2026ex991.htm"},{"name":"ex21.htm"},{"name":"EX-99_1.htm"},{"name":"image.jpg"}]}}"#;
        let names = exhibit_names(json).unwrap();
        assert_eq!(names, vec!["ex10-1.htm", "a2026ex991.htm", "EX-99_1.htm"]);
    }

    #[test]
    fn builds_urls() {
        let cik = Cik::new(1_467_858).unwrap();
        let f = filing("8-K", "2026-05-01", "0001467858-26-000012");
        assert_eq!(index_url(cik, &f), "https://www.sec.gov/Archives/edgar/data/1467858/000146785826000012/index.json");
        assert_eq!(file_url(cik, &f, "ex99.htm"), "https://www.sec.gov/Archives/edgar/data/1467858/000146785826000012/ex99.htm");
    }
}
