//! USGS earthquake feeds. No key. The data is produced by the US
//! government and is in the public domain.

use crate::{format_err, Http, Result, SourceError};
use serde::Deserialize;
use term_core::Event;

pub const FEEDS: &[&str] = &["significant_day", "4.5_day", "significant_week", "4.5_week", "significant_month", "4.5_month"];

pub fn url(feed: &str) -> Result<String> {
    if !FEEDS.contains(&feed) {
        return Err(SourceError::Config(format!("unknown feed {feed:?}. Choose one of {}", FEEDS.join(", "))));
    }
    Ok(format!("https://earthquake.usgs.gov/earthquakes/feed/v1.0/summary/{feed}.geojson"))
}

#[derive(Deserialize)]
struct Collection {
    features: Vec<Feature>,
}

#[derive(Deserialize)]
struct Feature {
    id: String,
    properties: Properties,
    geometry: Option<Geometry>,
}

#[derive(Deserialize)]
struct Properties {
    mag: Option<f64>,
    place: Option<String>,
    /// Milliseconds since 1970.
    time: i64,
    url: Option<String>,
    alert: Option<String>,
}

#[derive(Deserialize)]
struct Geometry {
    /// Longitude, latitude, depth in km.
    coordinates: Vec<f64>,
}

pub fn parse(json: &str) -> Result<Vec<Event>> {
    let collection: Collection = serde_json::from_str(json).map_err(format_err)?;
    Ok(collection
        .features
        .into_iter()
        .map(|f| {
            let coords = f.geometry.map(|g| g.coordinates).unwrap_or_default();
            let place = f.properties.place.unwrap_or_else(|| "unknown place".to_string());
            let title = match f.properties.mag {
                Some(mag) => format!("M {mag} {place}"),
                None => place,
            };
            Event {
                source: "usgs".into(),
                external_id: f.id,
                kind: "earthquake".into(),
                title,
                time: f.properties.time.div_euclid(1000),
                lon: coords.first().copied(),
                lat: coords.get(1).copied(),
                severity: f.properties.mag,
                severity_unit: f.properties.mag.map(|_| "magnitude".to_string()),
                alert: f.properties.alert,
                country: None,
                url: f.properties.url,
            }
        })
        .collect())
}

pub async fn fetch(http: &Http, feed: &str) -> Result<Vec<Event>> {
    parse(&http.get_text(&url(feed)?).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_recorded_feed() {
        let events = parse(&crate::fixture("usgs_4.5_day.geojson")).unwrap();
        assert_eq!(events.len(), 3);
        let first = &events[0];
        assert_eq!(first.source, "usgs");
        assert_eq!(first.external_id, "us6000ucz2");
        assert_eq!(first.kind, "earthquake");
        assert_eq!(first.severity, Some(4.7));
        assert_eq!(first.title, "M 4.7 South Sandwich Islands region");
        assert_eq!(first.time, 1_795_446_657);
        assert!(first.url.as_deref().unwrap().starts_with("https://earthquake.usgs.gov/"));
        for e in &events {
            let (lat, lon) = (e.lat.unwrap(), e.lon.unwrap());
            assert!((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon), "{lat} {lon}");
        }
    }

    #[test]
    fn only_known_feeds_get_an_address() {
        assert_eq!(url("4.5_week").unwrap(), "https://earthquake.usgs.gov/earthquakes/feed/v1.0/summary/4.5_week.geojson");
        assert!(url("../../etc").is_err());
    }

    #[test]
    fn broken_json_is_an_error() {
        assert!(parse("{}").is_err());
    }
}
