//! FRED from the Federal Reserve Bank of St. Louis: US rates and macro.
//! Needs a free key in `FRED_API_KEY`.
//!
//! STATUS: NOT RUN. No key was available. The address is confirmed to
//! reach FRED (it answers "api_key is not set" without one). The shape of
//! a successful answer is written from FRED's documentation and has not
//! been checked against a real response. Record one into
//! `fixtures/kallor/` on the first live run.

use crate::{format_err, plain_code, Http, Result, SourceError};
use serde::Deserialize;
use term_core::SeriesPoint;

/// The key travels in the address, as FRED requires. Never log this value.
pub fn url(series: &str, api_key: &str) -> Result<String> {
    let series = plain_code(series, "series")?;
    let key = plain_code(api_key, "key").map_err(|_| SourceError::Config("FRED_API_KEY has an unexpected form".into()))?;
    Ok(format!(
        "https://api.stlouisfed.org/fred/series/observations?series_id={series}&api_key={key}&file_type=json"
    ))
}

#[derive(Deserialize)]
struct Envelope {
    observations: Vec<Observation>,
}

#[derive(Deserialize)]
struct Observation {
    date: String,
    /// A number as text. A lone full stop means no value.
    value: String,
}

pub fn parse(json: &str) -> Result<Vec<SeriesPoint>> {
    let envelope: Envelope = serde_json::from_str(json).map_err(format_err)?;
    Ok(envelope
        .observations
        .into_iter()
        .filter_map(|o| o.value.trim().parse::<f64>().ok().map(|value| SeriesPoint { date: o.date, value }))
        .collect())
}

pub async fn fetch(http: &Http, series: &str) -> Result<Vec<SeriesPoint>> {
    let key = std::env::var("FRED_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
        .ok_or_else(|| SourceError::Config("FRED_API_KEY is not set. The key is free at fred.stlouisfed.org".into()))?;
    parse(&http.get_text(&url(series, &key)?).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_documented_shape_and_skips_missing_values() {
        // Hand written from the documentation. Not a recorded response.
        let json = r#"{"count": 3, "observations": [
            {"realtime_start": "2026-11-23", "realtime_end": "2026-11-23", "date": "2026-11-19", "value": "4.21"},
            {"realtime_start": "2026-11-23", "realtime_end": "2026-11-23", "date": "2026-11-20", "value": "."},
            {"realtime_start": "2026-11-23", "realtime_end": "2026-11-23", "date": "2026-11-23", "value": "4.25"}
        ]}"#;
        let points = parse(json).unwrap();
        assert_eq!(points.len(), 2);
        assert_eq!(points[1], SeriesPoint { date: "2026-11-23".into(), value: 4.25 });
    }

    #[test]
    fn the_real_error_answer_is_an_error() {
        // Recorded 2026-11-23 from a request without a key.
        let recorded = r#"{"error_code":400,"error_message":"Bad Request.  Variable api_key is not set.  Read https:\/\/fred.stlouisfed.org\/docs\/api\/api_key.html for more information."}"#;
        assert!(parse(recorded).is_err());
    }

    #[test]
    fn address_refuses_odd_input() {
        assert!(url("DGS10", "abcdef0123456789abcdef0123456789").unwrap().contains("series_id=DGS10&api_key="));
        assert!(url("DGS10&x=1", "abc").is_err());
        let err = url("DGS10", "bad key&").unwrap_err().to_string();
        assert!(!err.contains("bad key"), "the key must not be echoed: {err}");
    }
}
