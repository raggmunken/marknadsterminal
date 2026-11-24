//! Chain engine step 1: find and fetch documents from SEC EDGAR.
//!
//! STATUS: run live against EDGAR on 2026-11-23: the company list, the
//! submissions index, a full annual report and the full text search.

pub mod docs;
pub mod facts;
pub mod limiter;
pub mod search;

pub use limiter::RateLimiter;

use serde::{Deserialize, Serialize};
use term_core::Cik;

/// EDGAR allows at most ten requests per second. We stay under it.
pub const EDGAR_MAX_PER_SECOND: u32 = 8;

#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("user agent must name who you are and a contact address, for example \"Acme Research anna@acme.se\": {0}")]
    UserAgent(String),
    #[error("http: {0}")]
    Http(String),
    #[error("{url} answered {status}")]
    Status { url: String, status: u16 },
    #[error("unexpected submissions format: {0}")]
    Format(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Filing {
    pub accession: String,
    pub form: String,
    /// ISO date, as EDGAR gives it.
    pub filing_date: String,
    pub primary_document: String,
}

pub fn submissions_url(cik: Cik) -> String {
    format!("https://data.sec.gov/submissions/CIK{cik}.json")
}

pub fn document_url(cik: Cik, filing: &Filing) -> String {
    format!(
        "https://www.sec.gov/Archives/edgar/data/{}/{}/{}",
        cik.unpadded(),
        filing.accession.replace('-', ""),
        filing.primary_document
    )
}

#[derive(Deserialize)]
struct Submissions {
    filings: FilingsBlock,
}

#[derive(Deserialize)]
struct FilingsBlock {
    recent: Recent,
}

/// EDGAR stores the recent filings as parallel arrays, one per column.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Recent {
    accession_number: Vec<String>,
    filing_date: Vec<String>,
    form: Vec<String>,
    primary_document: Vec<String>,
}

/// Parse the `filings.recent` block of a submissions document.
pub fn parse_submissions(json: &str) -> Result<Vec<Filing>, IngestError> {
    let parsed: Submissions =
        serde_json::from_str(json).map_err(|e| IngestError::Format(e.to_string()))?;
    let r = parsed.filings.recent;
    let n = r.accession_number.len();
    if r.filing_date.len() != n || r.form.len() != n || r.primary_document.len() != n {
        return Err(IngestError::Format("columns differ in length".into()));
    }
    Ok((0..n)
        .map(|i| Filing {
            accession: r.accession_number[i].clone(),
            form: r.form[i].clone(),
            filing_date: r.filing_date[i].clone(),
            primary_document: r.primary_document[i].clone(),
        })
        .collect())
}

/// The newest filing of exactly the given form, for example `10-K`.
/// Amendments such as `10-K/A` are not matched.
pub fn latest_of_form<'a>(filings: &'a [Filing], form: &str) -> Option<&'a Filing> {
    filings
        .iter()
        .filter(|f| f.form == form && !f.primary_document.is_empty())
        .max_by(|a, b| a.filing_date.cmp(&b.filing_date))
}

/// SEC asks every automated client to say who it is and how to reach it.
pub fn validate_user_agent(user_agent: &str) -> Result<(), IngestError> {
    let ua = user_agent.trim();
    let has_contact = ua.split_whitespace().any(|part| {
        let part = part.trim_matches(|c: char| !c.is_alphanumeric());
        matches!(part.split_once('@'), Some((user, host)) if !user.is_empty() && host.contains('.'))
    });
    let placeholder = ["example.com", "example.org", "example.net", "domain.com"]
        .iter()
        .any(|p| ua.to_ascii_lowercase().contains(p));
    if ua.split_whitespace().count() < 2 || !has_contact || placeholder {
        return Err(IngestError::UserAgent(format!("{user_agent:?}")));
    }
    Ok(())
}

pub struct EdgarClient {
    http: reqwest::Client,
    limiter: RateLimiter,
}

impl EdgarClient {
    pub fn new(user_agent: &str) -> Result<Self, IngestError> {
        validate_user_agent(user_agent)?;
        let http = reqwest::Client::builder()
            .user_agent(user_agent.trim())
            .build()
            .map_err(|e| IngestError::Http(e.to_string()))?;
        Ok(Self { http, limiter: RateLimiter::per_second(EDGAR_MAX_PER_SECOND) })
    }

    pub async fn get_text(&self, url: &str) -> Result<String, IngestError> {
        // EDGAR now and then answers 500 on a request that works a moment later.
        // One retry after a pause, never more, and never on a refusal (4xx).
        for attempt in 0..2 {
            self.limiter.acquire().await;
            let response =
                self.http.get(url).send().await.map_err(|e| IngestError::Http(e.to_string()))?;
            let status = response.status();
            if status.is_server_error() && attempt == 0 {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                continue;
            }
            if !status.is_success() {
                return Err(IngestError::Status { url: url.to_string(), status: status.as_u16() });
            }
            return response.text().await.map_err(|e| IngestError::Http(e.to_string()));
        }
        unreachable!("the loop returns on its second pass")
    }

    pub async fn filings(&self, cik: Cik) -> Result<Vec<Filing>, IngestError> {
        parse_submissions(&self.get_text(&submissions_url(cik)).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand written in the shape of a submissions document. Not real data.
    const SUBMISSIONS: &str = r#"{
        "cik": "42",
        "name": "Synthetic Components Inc.",
        "filings": {
            "recent": {
                "accessionNumber": ["0000000042-26-000007", "0000000042-25-000031", "0000000042-25-000004", "0000000042-24-000005"],
                "filingDate": ["2026-02-11", "2025-08-01", "2025-02-12", "2024-02-14"],
                "form": ["10-K/A", "10-Q", "10-K", "10-K"],
                "primaryDocument": ["a10ka.htm", "q2.htm", "fy2024.htm", "fy2023.htm"],
                "isXBRL": [1, 1, 1, 1]
            },
            "files": []
        }
    }"#;

    #[test]
    fn builds_urls() {
        let cik = Cik::new(1_467_858).unwrap();
        assert_eq!(submissions_url(cik), "https://data.sec.gov/submissions/CIK0001467858.json");
        let filing = Filing {
            accession: "0001467858-25-000012".into(),
            form: "10-K".into(),
            filing_date: "2025-01-28".into(),
            primary_document: "doc.htm".into(),
        };
        assert_eq!(
            document_url(cik, &filing),
            "https://www.sec.gov/Archives/edgar/data/1467858/000146785825000012/doc.htm"
        );
    }

    #[test]
    fn parses_columns_into_filings() {
        let filings = parse_submissions(SUBMISSIONS).unwrap();
        assert_eq!(filings.len(), 4);
        assert_eq!(filings[1].form, "10-Q");
        assert_eq!(filings[1].primary_document, "q2.htm");
    }

    #[test]
    fn picks_newest_exact_form_and_ignores_amendments() {
        let filings = parse_submissions(SUBMISSIONS).unwrap();
        let latest = latest_of_form(&filings, "10-K").unwrap();
        assert_eq!(latest.accession, "0000000042-25-000004");
        assert!(latest_of_form(&filings, "20-F").is_none());
    }

    #[test]
    fn ragged_columns_are_an_error() {
        let json = r#"{"filings":{"recent":{"accessionNumber":["a","b"],"filingDate":["2025-01-01"],"form":["10-K","10-K"],"primaryDocument":["x","y"]}}}"#;
        assert!(matches!(parse_submissions(json), Err(IngestError::Format(_))));
        assert!(matches!(parse_submissions("not json"), Err(IngestError::Format(_))));
    }

    #[test]
    fn user_agent_needs_a_real_contact() {
        assert!(validate_user_agent("Acme Research anna@acme.se").is_ok());
        assert!(validate_user_agent("Acme Research (anna@acme.se)").is_ok());
        for bad in ["", "curl/8.0", "anna@acme.se", "Acme Research", "Acme test@example.com", "Acme a@b"] {
            assert!(validate_user_agent(bad).is_err(), "{bad:?} should be refused");
        }
        assert!(EdgarClient::new("Mozilla/5.0").is_err());
    }
}
