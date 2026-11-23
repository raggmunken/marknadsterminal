//! EPA Facility Registry Service: where facilities in the United States
//! are. No key. This is what lets an event be tied to a company's plants.
//!
//! The search is on the facility's registered name, which is often an old
//! or local name. Expect misses and false hits, and treat the result as a
//! starting list for a person to prune. It covers the United States only.

use crate::{format_err, Http, Result, SourceError};
use serde::Deserialize;
use term_core::Location;

pub const SOURCE: &str = "epa-frs";

/// Facilities whose name contains `name`, optionally within one state.
pub fn url(name: &str, state: Option<&str>) -> Result<String> {
    let name = name.trim();
    if name.chars().count() < 3 {
        return Err(SourceError::Config("the facility name must have at least three characters".into()));
    }
    let mut params = vec![("facility_name", name.to_uppercase()), ("output", "JSON".to_string())];
    if let Some(state) = state {
        let state = state.trim().to_uppercase();
        if state.len() != 2 || !state.bytes().all(|b| b.is_ascii_uppercase()) {
            return Err(SourceError::Config(format!("{state:?} is not a two letter state code")));
        }
        params.push(("state_abbr", state));
    }
    reqwest::Url::parse_with_params(
        "https://frs-public.epa.gov/ords/frs_public2/frs_rest_services.get_facilities",
        &params,
    )
    .map(String::from)
    .map_err(format_err)
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "Results")]
    results: Results,
}

#[derive(Deserialize)]
struct Results {
    #[serde(rename = "FRSFacility", default)]
    facilities: Vec<Facility>,
}

#[derive(Deserialize)]
struct Facility {
    #[serde(rename = "RegistryId")]
    registry_id: String,
    #[serde(rename = "FacilityName")]
    name: String,
    #[serde(rename = "CityName")]
    city: Option<String>,
    #[serde(rename = "StateAbbr")]
    state: Option<String>,
    /// Coordinates arrive as text and are often missing.
    #[serde(rename = "Latitude83")]
    lat: Option<String>,
    #[serde(rename = "Longitude83")]
    lon: Option<String>,
}

/// Facilities with coordinates, as places owned by `owner`. Facilities
/// without coordinates are of no use here and are skipped.
pub fn parse(json: &str, owner: &str) -> Result<Vec<Location>> {
    let envelope: Envelope = serde_json::from_str(json).map_err(format_err)?;
    Ok(envelope
        .results
        .facilities
        .into_iter()
        .filter_map(|f| {
            let lat: f64 = f.lat?.trim().parse().ok()?;
            let lon: f64 = f.lon?.trim().parse().ok()?;
            if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                return None;
            }
            let place = [f.city, f.state].into_iter().flatten().map(|s| s.trim().to_string()).collect::<Vec<_>>().join(", ");
            Some(Location {
                owner: owner.to_string(),
                label: if place.is_empty() { f.name.trim().to_string() } else { format!("{} ({place})", f.name.trim()) },
                lat,
                lon,
                source: SOURCE.to_string(),
                external_id: f.registry_id,
            })
        })
        .collect())
}

pub async fn fetch(http: &Http, search: &str, state: Option<&str>, owner: &str) -> Result<Vec<Location>> {
    parse(&http.get_text(&url(search, state)?).await?, owner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_recorded_facilities_and_skips_those_without_position() {
        let places = parse(&crate::fixture("epa_frs_facilities.json"), "General Motors Co").unwrap();
        assert_eq!(places.len(), 3, "four facilities recorded, one has no coordinates");
        let first = &places[0];
        assert_eq!(first.owner, "General Motors Co");
        assert_eq!(first.label, "B-O-C GROUP A DIV OF GENERAL MOTORS CORP (LANSING, MI)");
        assert_eq!((first.lat, first.lon), (42.72418, -84.55554));
        assert_eq!(first.source, "epa-frs");
        assert_eq!(first.external_id, "110072282454");
    }

    #[test]
    fn address_is_built_with_care() {
        let u = url("General Motors", Some("mi")).unwrap();
        assert!(u.contains("facility_name=GENERAL+MOTORS") && u.contains("state_abbr=MI") && u.contains("output=JSON"), "{u}");
        assert!(url("GM", None).is_err());
        assert!(url("General Motors", Some("Michigan")).is_err());
    }

    #[test]
    fn empty_answer_is_an_empty_list() {
        assert!(parse(r#"{"Results": {"FRSFacility": []}}"#, "X").unwrap().is_empty());
        assert!(parse(r#"{"Results": {}}"#, "X").unwrap().is_empty());
    }
}
