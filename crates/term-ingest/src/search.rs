//! Chain engine step 6: reverse search. "Who else mentions this company?"
//!
//! STATUS: UNVERIFIED. EDGAR full text search has no published contract.
//! The address and the response shape below are written from how the
//! search page is known to behave and have never been checked against a
//! live answer from this code. The parser accepts missing fields for that
//! reason. The first live run must save a real response as a test file.
//! The search covers filings from 2001 onwards.

use crate::{EdgarClient, IngestError};
use serde::Deserialize;
use term_core::Cik;

pub const FULL_TEXT_ENDPOINT: &str = "https://efts.sec.gov/LATEST/search-index";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// Companies the filing belongs to.
    pub ciks: Vec<Cik>,
    pub display_names: Vec<String>,
    pub form: Option<String>,
    pub file_date: Option<String>,
    pub accession: Option<String>,
}

/// Address for an exact phrase search, limited to the given form types.
pub fn full_text_url(phrase: &str, forms: &[&str]) -> Result<String, IngestError> {
    let phrase = phrase.trim().replace('"', " ");
    if phrase.is_empty() {
        return Err(IngestError::Format("empty search phrase".into()));
    }
    let quoted = format!("\"{phrase}\"");
    let mut params = vec![("q", quoted)];
    if !forms.is_empty() {
        params.push(("forms", forms.join(",")));
    }
    reqwest::Url::parse_with_params(FULL_TEXT_ENDPOINT, &params)
        .map(String::from)
        .map_err(|e| IngestError::Format(e.to_string()))
}

#[derive(Deserialize)]
struct Envelope {
    hits: Outer,
}

#[derive(Deserialize)]
struct Outer {
    #[serde(default)]
    hits: Vec<RawHit>,
}

#[derive(Deserialize)]
struct RawHit {
    #[serde(rename = "_source", default)]
    source: Source,
}

#[derive(Deserialize, Default)]
struct Source {
    #[serde(default)]
    ciks: Vec<String>,
    #[serde(default)]
    display_names: Vec<String>,
    #[serde(default)]
    form: Option<String>,
    #[serde(default)]
    file_date: Option<String>,
    #[serde(default)]
    adsh: Option<String>,
}

pub fn parse_full_text(json: &str) -> Result<Vec<SearchHit>, IngestError> {
    let envelope: Envelope =
        serde_json::from_str(json).map_err(|e| IngestError::Format(e.to_string()))?;
    Ok(envelope
        .hits
        .hits
        .into_iter()
        .map(|h| SearchHit {
            // A CIK that does not parse is dropped, the hit is kept.
            ciks: h.source.ciks.iter().filter_map(|c| c.parse().ok()).collect(),
            display_names: h.source.display_names,
            form: h.source.form,
            file_date: h.source.file_date,
            accession: h.source.adsh,
        })
        .collect())
}

/// The companies that mention the phrase, without the company itself.
pub fn mentioning_companies(hits: &[SearchHit], exclude: Cik) -> Vec<Cik> {
    let mut out: Vec<Cik> = hits.iter().flat_map(|h| h.ciks.iter().copied()).filter(|c| *c != exclude).collect();
    out.sort();
    out.dedup();
    out
}

impl EdgarClient {
    pub async fn full_text_search(&self, phrase: &str, forms: &[&str]) -> Result<Vec<SearchHit>, IngestError> {
        parse_full_text(&self.get_text(&full_text_url(phrase, forms)?).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand written in the assumed shape. Not a recorded response.
    const ASSUMED: &str = r#"{
        "hits": {
            "total": {"value": 2, "relation": "eq"},
            "hits": [
                {"_id": "0000000020-25-000009:fy2024.htm",
                 "_source": {"ciks": ["0000000020"], "display_names": ["Polarvik Castings Ltd (CIK 0000000020)"],
                             "form": "10-K", "file_date": "2025-03-01", "adsh": "0000000020-25-000009"}},
                {"_id": "x", "_source": {"ciks": ["0000000042", "not-a-cik"], "display_names": []}},
                {"_id": "y"}
            ]
        }
    }"#;

    #[test]
    fn url_quotes_the_phrase_and_lists_forms() {
        let url = full_text_url("Nordvik Test Components", &["10-K", "20-F"]).unwrap();
        assert_eq!(
            url,
            "https://efts.sec.gov/LATEST/search-index?q=%22Nordvik+Test+Components%22&forms=10-K%2C20-F"
        );
        assert!(full_text_url("AT&T Inc", &[]).unwrap().ends_with("?q=%22AT%26T+Inc%22"));
        assert!(full_text_url("  ", &[]).is_err());
    }

    #[test]
    fn embedded_quotes_cannot_break_out_of_the_phrase() {
        let url = full_text_url("Acme\" OR \"Other", &[]).unwrap();
        assert_eq!(url.matches("%22").count(), 2);
    }

    #[test]
    fn parses_hits_and_tolerates_missing_fields() {
        let hits = parse_full_text(ASSUMED).unwrap();
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].ciks, vec![Cik::new(20).unwrap()]);
        assert_eq!(hits[0].form.as_deref(), Some("10-K"));
        assert_eq!(hits[0].accession.as_deref(), Some("0000000020-25-000009"));
        assert_eq!(hits[1].ciks, vec![Cik::new(42).unwrap()]);
        assert!(hits[2].ciks.is_empty());
        assert!(parse_full_text("{}").is_err());
    }

    #[test]
    fn mentioning_companies_excludes_the_subject() {
        let hits = parse_full_text(ASSUMED).unwrap();
        assert_eq!(mentioning_companies(&hits, Cik::new(42).unwrap()), vec![Cik::new(20).unwrap()]);
    }
}
