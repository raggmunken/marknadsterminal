//! ECB euro reference exchange rates. No key. One rate per business day,
//! quoted as units of the currency per euro.

use crate::{format_err, Http, Result, SourceError};
use term_core::SeriesPoint;

pub fn url(currency: &str, last_n: u32) -> Result<String> {
    let c = currency.trim().to_uppercase();
    if c.len() != 3 || !c.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(SourceError::Config(format!("{currency:?} is not a three letter currency code")));
    }
    Ok(format!(
        "https://data-api.ecb.europa.eu/service/data/EXR/D.{c}.EUR.SP00.A?lastNObservations={}&format=csvdata",
        last_n.clamp(1, 10_000)
    ))
}

pub fn series_id(currency: &str) -> String {
    format!("EXR.D.{}.EUR", currency.trim().to_uppercase())
}

/// Reads the ECB's CSV. Columns are found by name, since the ECB adds
/// columns now and then.
pub fn parse(csv_text: &str) -> Result<Vec<SeriesPoint>> {
    let mut reader = csv::Reader::from_reader(csv_text.as_bytes());
    let headers = reader.headers().map_err(format_err)?.clone();
    let column = |name: &str| {
        headers.iter().position(|h| h == name).ok_or_else(|| format_err(format!("no column {name}")))
    };
    let (date_col, value_col) = (column("TIME_PERIOD")?, column("OBS_VALUE")?);
    let mut points = Vec::new();
    for row in reader.records() {
        let row = row.map_err(format_err)?;
        let (Some(date), Some(value)) = (row.get(date_col), row.get(value_col)) else { continue };
        // Days without a fixing have an empty value.
        if let Ok(value) = value.trim().parse::<f64>() {
            points.push(SeriesPoint { date: date.to_string(), value });
        }
    }
    Ok(points)
}

pub async fn fetch(http: &Http, currency: &str, last_n: u32) -> Result<Vec<SeriesPoint>> {
    parse(&http.get_text(&url(currency, last_n)?).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_recorded_rates() {
        let points = parse(&crate::fixture("ecb_exr.csv")).unwrap();
        assert_eq!(
            points,
            vec![
                SeriesPoint { date: "2026-11-19".into(), value: 11.147 },
                SeriesPoint { date: "2026-11-20".into(), value: 11.2125 },
                SeriesPoint { date: "2026-11-23".into(), value: 11.2375 },
            ]
        );
    }

    #[test]
    fn empty_values_are_skipped_and_missing_columns_fail() {
        let points = parse("TIME_PERIOD,OBS_VALUE\n2026-01-01,\n2026-01-02,11.5\n").unwrap();
        assert_eq!(points.len(), 1);
        assert!(parse("A,B\n1,2\n").is_err());
    }

    #[test]
    fn address_and_series_id() {
        assert!(url("sek", 5).unwrap().contains("/EXR/D.SEK.EUR.SP00.A?lastNObservations=5"));
        assert!(url("kronor", 5).is_err());
        assert_eq!(series_id("sek"), "EXR.D.SEK.EUR");
    }
}
