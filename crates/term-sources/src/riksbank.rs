//! Sveriges Riksbank, interest rates and exchange rates. No key is needed
//! for light use. Series are named by the Riksbank, for example
//! `SEKUSDPMI` (kronor per US dollar) and `SECBREPOEFF` (the policy rate).

use crate::{format_err, plain_code, Http, Result, SourceError};
use term_core::time::iso_to_unix;
use term_core::SeriesPoint;

pub fn url(series: &str, from: &str, to: &str) -> Result<String> {
    let series = plain_code(series, "series")?;
    for date in [from, to] {
        if date.len() != 10 || iso_to_unix(date).is_none() {
            return Err(SourceError::Config(format!("{date:?} is not a date (YYYY-MM-DD)")));
        }
    }
    Ok(format!("https://api.riksbank.se/swea/v1/Observations/{series}/{from}/{to}"))
}

pub fn parse(json: &str) -> Result<Vec<SeriesPoint>> {
    serde_json::from_str(json).map_err(format_err)
}

pub async fn fetch(http: &Http, series: &str, from: &str, to: &str) -> Result<Vec<SeriesPoint>> {
    parse(&http.get_text(&url(series, from, to)?).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_recorded_observations() {
        let points = parse(&crate::fixture("riksbank_observations.json")).unwrap();
        assert_eq!(points.len(), 6);
        assert_eq!(points[0], SeriesPoint { date: "2026-11-16".into(), value: 10.07354 });
        assert_eq!(points[5], SeriesPoint { date: "2026-11-23".into(), value: 10.27288 });
        assert!(points.windows(2).all(|w| w[0].date < w[1].date), "dates are in order");
    }

    #[test]
    fn address_checks_series_and_dates() {
        assert_eq!(
            url("SEKUSDPMI", "2026-11-16", "2026-11-23").unwrap(),
            "https://api.riksbank.se/swea/v1/Observations/SEKUSDPMI/2026-11-16/2026-11-23"
        );
        assert!(url("SEK/USD", "2026-11-16", "2026-11-23").is_err());
        assert!(url("SEKUSDPMI", "16 nov", "2026-11-23").is_err());
    }
}
