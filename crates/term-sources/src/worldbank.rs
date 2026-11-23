//! World Bank indicators. No key. Annual macro series per country, for
//! example `NY.GDP.MKTP.CD` (GDP in current US dollars).

use crate::{format_err, plain_code, Http, Result};
use serde::Deserialize;
use serde_json::Value;
use term_core::SeriesPoint;

pub fn url(country: &str, indicator: &str, per_page: u32) -> Result<String> {
    let country = plain_code(country, "country")?;
    let indicator = plain_code(indicator, "indicator")?;
    Ok(format!(
        "https://api.worldbank.org/v2/country/{country}/indicator/{indicator}?format=json&per_page={}",
        per_page.clamp(1, 1000)
    ))
}

pub fn series_id(country: &str, indicator: &str) -> String {
    format!("{}.{}", country.trim().to_uppercase(), indicator.trim())
}

#[derive(Deserialize)]
struct Row {
    date: String,
    value: Option<f64>,
}

/// The answer is a two element array: paging information, then the rows.
/// Years without a value are skipped. The result is oldest first.
pub fn parse(json: &str) -> Result<Vec<SeriesPoint>> {
    let outer: Vec<Value> = serde_json::from_str(json).map_err(format_err)?;
    let rows = match outer.get(1) {
        Some(Value::Null) | None => return Err(format_err("the answer holds no data, check country and indicator")),
        Some(rows) => rows.clone(),
    };
    let rows: Vec<Row> = serde_json::from_value(rows).map_err(format_err)?;
    let mut points: Vec<SeriesPoint> =
        rows.into_iter().filter_map(|r| r.value.map(|value| SeriesPoint { date: r.date, value })).collect();
    points.sort_by(|a, b| a.date.cmp(&b.date));
    Ok(points)
}

pub async fn fetch(http: &Http, country: &str, indicator: &str, per_page: u32) -> Result<Vec<SeriesPoint>> {
    parse(&http.get_text(&url(country, indicator, per_page)?).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_recorded_indicator() {
        let points = parse(&crate::fixture("worldbank_indicator.json")).unwrap();
        assert_eq!(points.len(), 3);
        assert_eq!(points[0].date, "2023");
        assert_eq!(points[2].date, "2025");
        assert!((points[2].value - 668_998_664_082.081).abs() < 1.0);
    }

    #[test]
    fn error_answers_are_errors() {
        // The World Bank answers an unknown indicator with a one element array.
        assert!(parse(r#"[{"message":[{"id":"120","key":"Invalid value"}]}]"#).is_err());
        assert!(parse(r#"[{"page":1},null]"#).is_err());
    }

    #[test]
    fn null_years_are_skipped() {
        let points = parse(r#"[{}, [{"date":"2025","value":null},{"date":"2024","value":2.5}]]"#).unwrap();
        assert_eq!(points, vec![SeriesPoint { date: "2024".into(), value: 2.5 }]);
    }
}
