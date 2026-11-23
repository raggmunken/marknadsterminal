//! filings.xbrl.org: annual reports from European listed companies in the
//! ESEF format, collected by XBRL International. No key.
//!
//! This is the European counterpart of step 1. Companies are identified
//! by LEI, not CIK, which is why `gleif` is needed alongside.

use crate::{format_err, plain_code, Http, Result};
use serde::Deserialize;
use std::collections::HashMap;

pub const BASE: &str = "https://filings.xbrl.org";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EsefFiling {
    pub lei: String,
    pub entity_name: Option<String>,
    pub country: String,
    pub period_end: String,
    pub published: Option<String>,
    /// Full address of the human readable report.
    pub report_url: Option<String>,
    /// Full address of the facts as JSON.
    pub json_url: Option<String>,
    pub error_count: u32,
}

/// Newest filings first for one country (two letter code, for example `SE`).
pub fn url(country: &str, limit: u32) -> Result<String> {
    let country = plain_code(country, "country")?.to_uppercase();
    Ok(format!(
        "{BASE}/api/filings?filter%5Bcountry%5D={country}&page%5Bsize%5D={}&include=entity&sort=-date_added",
        limit.clamp(1, 200)
    ))
}

#[derive(Deserialize)]
struct Envelope {
    data: Vec<Item>,
    #[serde(default)]
    included: Vec<Included>,
}

#[derive(Deserialize)]
struct Item {
    attributes: Attributes,
}

#[derive(Deserialize)]
struct Attributes {
    period_end: String,
    country: String,
    /// Starts with the LEI: `<LEI>-<period>-ESEF-<country>-<n>`.
    fxo_id: String,
    published: Option<String>,
    report_url: Option<String>,
    json_url: Option<String>,
    #[serde(default)]
    error_count: u32,
}

#[derive(Deserialize)]
struct Included {
    #[serde(rename = "type")]
    kind: String,
    attributes: IncludedAttributes,
}

#[derive(Deserialize)]
struct IncludedAttributes {
    identifier: Option<String>,
    name: Option<String>,
}

fn absolute(path: Option<String>) -> Option<String> {
    path.filter(|p| !p.is_empty()).map(|p| if p.starts_with("http") { p } else { format!("{BASE}{p}") })
}

pub fn parse(json: &str) -> Result<Vec<EsefFiling>> {
    let envelope: Envelope = serde_json::from_str(json).map_err(format_err)?;
    let names: HashMap<String, String> = envelope
        .included
        .into_iter()
        .filter(|i| i.kind == "entity")
        .filter_map(|i| Some((i.attributes.identifier?, i.attributes.name?)))
        .collect();
    envelope
        .data
        .into_iter()
        .map(|item| {
            let a = item.attributes;
            let lei = a.fxo_id.split('-').next().unwrap_or_default().to_string();
            if lei.len() != 20 {
                return Err(format_err(format!("filing id {:?} does not start with an LEI", a.fxo_id)));
            }
            Ok(EsefFiling {
                entity_name: names.get(&lei).cloned(),
                lei,
                country: a.country,
                period_end: a.period_end,
                published: a.published,
                report_url: absolute(a.report_url),
                json_url: absolute(a.json_url),
                error_count: a.error_count,
            })
        })
        .collect()
}

pub async fn latest(http: &Http, country: &str, limit: u32) -> Result<Vec<EsefFiling>> {
    parse(&http.get_text(&url(country, limit)?).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_recorded_filings() {
        let filings = parse(&crate::fixture("esef_filings.json")).unwrap();
        assert_eq!(filings.len(), 2);
        let first = &filings[0];
        assert_eq!(first.lei, "549300C3IRYAOWOT0783");
        assert_eq!(first.entity_name.as_deref(), Some("Dustin Group AB"));
        assert_eq!(first.country, "SE");
        assert_eq!(first.period_end, "2026-08-31");
        assert_eq!(first.error_count, 0);
        let report = first.report_url.as_deref().unwrap();
        assert!(report.starts_with("https://filings.xbrl.org/549300C3IRYAOWOT0783/") && report.ends_with(".xhtml"));
        assert!(first.json_url.as_deref().unwrap().ends_with(".json"));
        assert!(filings.iter().all(|f| f.country == "SE" && f.lei.len() == 20));
    }

    #[test]
    fn address_filters_on_country() {
        let u = url("se", 3).unwrap();
        assert!(u.contains("filter%5Bcountry%5D=SE") && u.contains("page%5Bsize%5D=3"), "{u}");
        assert!(url("S E", 3).is_err());
    }
}
