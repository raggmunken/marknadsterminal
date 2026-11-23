//! GDACS, the Global Disaster Alert and Coordination System. No key.
//! Covers earthquakes, cyclones, floods, volcanoes, droughts and wildfires
//! with an alert level.

use crate::{format_err, Http, Result};
use serde::Deserialize;
use term_core::time::iso_to_unix;
use term_core::Event;

/// Orange and red alerts for the event types that can stop a factory or a port.
pub const URL: &str =
    "https://www.gdacs.org/gdacsapi/api/events/geteventlist/SEARCH?eventlist=EQ;TC;FL;VO;WF&alertlevel=Orange;Red";

#[derive(Deserialize)]
struct Collection {
    features: Vec<Feature>,
}

#[derive(Deserialize)]
struct Feature {
    properties: Properties,
    geometry: Option<Geometry>,
}

#[derive(Deserialize)]
struct Properties {
    eventtype: String,
    eventid: i64,
    name: Option<String>,
    alertlevel: Option<String>,
    country: Option<String>,
    fromdate: String,
    url: Option<Links>,
    severitydata: Option<Severity>,
}

#[derive(Deserialize)]
struct Links {
    report: Option<String>,
}

#[derive(Deserialize)]
struct Severity {
    severity: Option<f64>,
    severityunit: Option<String>,
}

#[derive(Deserialize)]
struct Geometry {
    #[serde(default)]
    coordinates: serde_json::Value,
}

fn kind(code: &str) -> String {
    match code {
        "EQ" => "earthquake",
        "TC" => "cyclone",
        "FL" => "flood",
        "VO" => "volcano",
        "DR" => "drought",
        "WF" => "wildfire",
        other => return other.to_lowercase(),
    }
    .to_string()
}

pub fn parse(json: &str) -> Result<Vec<Event>> {
    let collection: Collection = serde_json::from_str(json).map_err(format_err)?;
    let mut events = Vec::with_capacity(collection.features.len());
    for f in collection.features {
        let p = f.properties;
        let time = iso_to_unix(&p.fromdate)
            .ok_or_else(|| format_err(format!("event {} has date {:?}", p.eventid, p.fromdate)))?;
        // Only point geometries carry a usable position.
        let point: Vec<f64> = f
            .geometry
            .and_then(|g| serde_json::from_value(g.coordinates).ok())
            .unwrap_or_default();
        let (severity, severity_unit) = match p.severitydata {
            Some(s) => (s.severity, s.severityunit.filter(|u| !u.trim().is_empty())),
            None => (None, None),
        };
        events.push(Event {
            source: "gdacs".into(),
            external_id: format!("{}{}", p.eventtype, p.eventid),
            kind: kind(&p.eventtype),
            title: p.name.unwrap_or_else(|| format!("{} {}", p.eventtype, p.eventid)),
            time,
            lon: point.first().copied(),
            lat: point.get(1).copied(),
            severity,
            severity_unit,
            alert: p.alertlevel,
            country: p.country.filter(|c| !c.trim().is_empty()),
            url: p.url.and_then(|u| u.report),
        });
    }
    Ok(events)
}

pub async fn fetch(http: &Http) -> Result<Vec<Event>> {
    parse(&http.get_text(URL).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_recorded_alerts() {
        let events = parse(&crate::fixture("gdacs_events.json")).unwrap();
        assert_eq!(events.len(), 3);
        let first = &events[0];
        assert_eq!(first.source, "gdacs");
        assert_eq!(first.external_id, "TC1001346");
        assert_eq!(first.kind, "cyclone");
        assert_eq!(first.title, "Tropical Cyclone MURASU-26");
        assert_eq!(first.alert.as_deref(), Some("Red"));
        assert_eq!(first.country.as_deref(), Some("India"));
        assert_eq!(first.time, iso_to_unix("2026-11-01T00:00:00").unwrap());
        assert_eq!((first.lon, first.lat), (Some(80.2), Some(15.8)));
        assert_eq!(first.severity_unit.as_deref(), Some("km/h"));
        assert!(first.url.as_deref().unwrap().contains("report.aspx"));
        for e in &events {
            assert!(matches!(e.alert.as_deref(), Some("Orange" | "Red")), "{:?}", e.alert);
        }
    }

    #[test]
    fn unknown_type_keeps_its_code() {
        assert_eq!(kind("XX"), "xx");
        assert_eq!(kind("FL"), "flood");
    }
}
