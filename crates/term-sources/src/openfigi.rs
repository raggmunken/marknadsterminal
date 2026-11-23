//! OpenFIGI: maps tickers to FIGI instrument identifiers. Works without a
//! key at a lower rate limit. Set `OPENFIGI_API_KEY` to raise it.

use crate::{format_err, Http, Result, SourceError};
use serde::Deserialize;
use serde_json::{json, Value};

pub const URL: &str = "https://api.openfigi.com/v3/mapping";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FigiRecord {
    pub figi: String,
    pub name: Option<String>,
    pub ticker: Option<String>,
    #[serde(rename = "exchCode")]
    pub exch_code: Option<String>,
    #[serde(rename = "securityType")]
    pub security_type: Option<String>,
    #[serde(rename = "compositeFIGI")]
    pub composite_figi: Option<String>,
    #[serde(rename = "shareClassFIGI")]
    pub share_class_figi: Option<String>,
}

/// One mapping job per ticker. `exch_code` is OpenFIGI's exchange code,
/// for example `US` for the US composite or `SS` for Stockholm.
pub fn request_body(jobs: &[(&str, &str)]) -> Result<Value> {
    if jobs.is_empty() || jobs.len() > 10 {
        return Err(SourceError::Config("send between 1 and 10 tickers at a time".into()));
    }
    Ok(Value::Array(
        jobs.iter()
            .map(|(ticker, exch)| json!({"idType": "TICKER", "idValue": ticker.trim().to_uppercase(), "exchCode": exch.trim().to_uppercase()}))
            .collect(),
    ))
}

#[derive(Deserialize)]
struct JobResult {
    #[serde(default)]
    data: Vec<FigiRecord>,
}

/// One list per job, in the order the jobs were sent. A ticker that is
/// not found gives an empty list.
pub fn parse(json: &str) -> Result<Vec<Vec<FigiRecord>>> {
    let results: Vec<JobResult> = serde_json::from_str(json).map_err(format_err)?;
    Ok(results.into_iter().map(|r| r.data).collect())
}

pub async fn map_tickers(http: &Http, jobs: &[(&str, &str)]) -> Result<Vec<Vec<FigiRecord>>> {
    let body = request_body(jobs)?;
    let key = std::env::var("OPENFIGI_API_KEY").ok().filter(|k| !k.trim().is_empty());
    let headers: Vec<(&str, &str)> = match &key {
        Some(k) => vec![("X-OPENFIGI-APIKEY", k.as_str())],
        None => Vec::new(),
    };
    parse(&http.post_json(URL, &body, &headers).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_recorded_mapping_with_one_hit_and_one_miss() {
        let results = parse(&crate::fixture("openfigi_mapping.json")).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].len(), 1);
        let gm = &results[0][0];
        assert_eq!(gm.figi, "BBG000NDYB67");
        assert_eq!(gm.name.as_deref(), Some("GENERAL MOTORS CO"));
        assert_eq!(gm.exch_code.as_deref(), Some("US"));
        assert_eq!(gm.security_type.as_deref(), Some("Common Stock"));
        assert_eq!(gm.share_class_figi.as_deref(), Some("BBG001SM1DK6"));
        assert!(results[1].is_empty(), "the unknown ticker gives no records");
    }

    #[test]
    fn body_has_one_job_per_ticker() {
        let body = request_body(&[(" gm ", "us"), ("VOLV-B", "SS")]).unwrap();
        assert_eq!(body[0], json!({"idType": "TICKER", "idValue": "GM", "exchCode": "US"}));
        assert_eq!(body[1]["exchCode"], "SS");
        assert!(request_body(&[]).is_err());
    }
}
