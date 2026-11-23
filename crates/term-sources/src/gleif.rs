//! GLEIF: the global register of legal entity identifiers (LEI). No key.
//! The LEI is the join key to European filings, which carry no CIK.

use crate::{format_err, Http, Result, SourceError};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeiRecord {
    pub lei: String,
    pub legal_name: String,
    pub jurisdiction: Option<String>,
    pub country: Option<String>,
    pub entity_status: Option<String>,
    pub registration_status: Option<String>,
}

/// Search by legal name. GLEIF matches the words of the name.
pub fn search_url(name: &str, limit: u32) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(SourceError::Config("empty company name".into()));
    }
    reqwest::Url::parse_with_params(
        "https://api.gleif.org/api/v1/lei-records",
        &[("filter[entity.legalName]", name.to_string()), ("page[size]", limit.clamp(1, 50).to_string())],
    )
    .map(String::from)
    .map_err(format_err)
}

#[derive(Deserialize)]
struct Envelope {
    data: Vec<Record>,
}

#[derive(Deserialize)]
struct Record {
    attributes: Attributes,
}

#[derive(Deserialize)]
struct Attributes {
    lei: String,
    entity: Entity,
    registration: Option<Registration>,
}

#[derive(Deserialize)]
struct Entity {
    #[serde(rename = "legalName")]
    legal_name: Name,
    jurisdiction: Option<String>,
    status: Option<String>,
    #[serde(rename = "legalAddress")]
    legal_address: Option<Address>,
}

#[derive(Deserialize)]
struct Name {
    name: String,
}

#[derive(Deserialize)]
struct Address {
    country: Option<String>,
}

#[derive(Deserialize)]
struct Registration {
    status: Option<String>,
}

pub fn parse(json: &str) -> Result<Vec<LeiRecord>> {
    let envelope: Envelope = serde_json::from_str(json).map_err(format_err)?;
    Ok(envelope
        .data
        .into_iter()
        .map(|r| LeiRecord {
            lei: r.attributes.lei,
            legal_name: r.attributes.entity.legal_name.name,
            jurisdiction: r.attributes.entity.jurisdiction,
            country: r.attributes.entity.legal_address.and_then(|a| a.country),
            entity_status: r.attributes.entity.status,
            registration_status: r.attributes.registration.and_then(|r| r.status),
        })
        .collect())
}

pub async fn search(http: &Http, name: &str, limit: u32) -> Result<Vec<LeiRecord>> {
    parse(&http.get_text(&search_url(name, limit)?).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_recorded_records() {
        let records = parse(&crate::fixture("gleif_lei_records.json")).unwrap();
        assert_eq!(records.len(), 2);
        let gm = &records[0];
        assert_eq!(gm.lei, "54930070NSV60J38I987");
        assert_eq!(gm.legal_name, "GENERAL MOTORS COMPANY");
        assert_eq!(gm.jurisdiction.as_deref(), Some("US-DE"));
        assert_eq!(gm.country.as_deref(), Some("US"));
        assert_eq!(gm.entity_status.as_deref(), Some("ACTIVE"));
        assert_eq!(gm.registration_status.as_deref(), Some("ISSUED"));
        assert!(records.iter().all(|r| r.lei.len() == 20));
    }

    #[test]
    fn address_encodes_the_name() {
        let url = search_url("AT&T Inc", 500).unwrap();
        assert!(url.contains("AT%26T+Inc"), "{url}");
        assert!(url.ends_with("page%5Bsize%5D=50"), "{url}");
        assert!(search_url(" ", 5).is_err());
    }
}
