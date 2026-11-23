//! What the terminal can see in each country of the world.
//!
//! The country list and the economy figures come from the World Bank.
//! For every country the two company registers that span borders are
//! asked how much they hold: GLEIF (companies with an identifier) and
//! filings.xbrl.org (annual reports in the European format). The numbers
//! are counted by the sources themselves. Nothing here is typed in by hand
//! except the short list of national filing systems the project has
//! actually looked at.

use crate::{format_err, plain_code, Http, Result};
use serde::Deserialize;
use serde_json::Value;
use term_core::Country;

pub const COUNTRIES_URL: &str = "https://api.worldbank.org/v2/country?format=json&per_page=400";
/// Most recent value per country.
pub const GDP_URL: &str =
    "https://api.worldbank.org/v2/country/all/indicator/NY.GDP.MKTP.CD?format=json&per_page=400&mrnev=1";

#[derive(Deserialize)]
struct WbCountry {
    id: String,
    #[serde(rename = "iso2Code")]
    iso2: String,
    name: String,
    region: Named,
    #[serde(rename = "incomeLevel")]
    income: Named,
    #[serde(rename = "capitalCity")]
    capital: String,
    latitude: String,
    longitude: String,
}

#[derive(Deserialize)]
struct Named {
    value: String,
}

fn second_element(json: &str) -> Result<Value> {
    let outer: Vec<Value> = serde_json::from_str(json).map_err(format_err)?;
    outer.into_iter().nth(1).filter(|v| !v.is_null()).ok_or_else(|| format_err("the World Bank answer holds no rows"))
}

/// Real countries and territories. The World Bank's own totals for regions
/// and income groups are left out.
pub fn parse_countries(json: &str) -> Result<Vec<Country>> {
    let rows: Vec<WbCountry> = serde_json::from_value(second_element(json)?).map_err(format_err)?;
    Ok(rows
        .into_iter()
        .filter(|r| r.region.value.trim() != "Aggregates")
        .map(|r| Country {
            iso2: r.iso2,
            iso3: r.id,
            name: r.name,
            region: r.region.value.trim().to_string(),
            income: r.income.value.trim().to_string(),
            capital: Some(r.capital).filter(|c| !c.trim().is_empty()),
            lat: r.latitude.trim().parse().ok(),
            lon: r.longitude.trim().parse().ok(),
            gdp_usd: None,
            gdp_year: None,
            lei_count: None,
            esef_count: None,
        })
        .collect())
}

#[derive(Deserialize)]
struct GdpRow {
    countryiso3code: String,
    date: String,
    value: Option<f64>,
}

/// (ISO3 code, year, GDP in current US dollars).
pub fn parse_gdp(json: &str) -> Result<Vec<(String, u16, f64)>> {
    let rows: Vec<GdpRow> = serde_json::from_value(second_element(json)?).map_err(format_err)?;
    Ok(rows
        .into_iter()
        .filter_map(|r| Some((r.countryiso3code, r.date.parse().ok()?, r.value?)))
        .filter(|(code, _, _)| !code.is_empty())
        .collect())
}

pub fn lei_count_url(iso2: &str) -> Result<String> {
    let code = plain_code(iso2, "country")?.to_uppercase();
    Ok(format!(
        "https://api.gleif.org/api/v1/lei-records?filter%5Bentity.legalAddress.country%5D={code}&page%5Bsize%5D=1"
    ))
}

pub fn parse_lei_count(json: &str) -> Result<u64> {
    let v: Value = serde_json::from_str(json).map_err(format_err)?;
    v.pointer("/meta/pagination/total").and_then(Value::as_u64).ok_or_else(|| format_err("no total in the GLEIF answer"))
}

pub fn esef_count_url(iso2: &str) -> Result<String> {
    let code = plain_code(iso2, "country")?.to_uppercase();
    Ok(format!("https://filings.xbrl.org/api/filings?filter%5Bcountry%5D={code}&page%5Bsize%5D=1"))
}

pub fn parse_esef_count(json: &str) -> Result<u64> {
    let v: Value = serde_json::from_str(json).map_err(format_err)?;
    v.pointer("/meta/count").and_then(Value::as_u64).ok_or_else(|| format_err("no count in the answer"))
}

/// National filing systems the project has looked at, by ISO2 code. Kept
/// short on purpose: a system is only listed once someone has checked it.
pub fn national_system(iso2: &str) -> Option<(&'static str, &'static str)> {
    match iso2 {
        "US" => Some(("SEC EDGAR", "inlagd och körd")),
        "JP" => Some(("EDINET", "kräver gratis nyckel, inte inlagd")),
        "KR" => Some(("Open DART", "kräver gratis nyckel, inte inlagd")),
        _ => None,
    }
}

pub async fn fetch_countries(http: &Http) -> Result<Vec<Country>> {
    let mut countries = parse_countries(&http.get_text(COUNTRIES_URL).await?)?;
    let gdp = parse_gdp(&http.get_text(GDP_URL).await?)?;
    for c in &mut countries {
        if let Some((_, year, value)) = gdp.iter().find(|(code, _, _)| *code == c.iso3) {
            c.gdp_usd = Some(*value);
            c.gdp_year = Some(*year);
        }
    }
    Ok(countries)
}

pub async fn lei_count(http: &Http, iso2: &str) -> Result<u64> {
    parse_lei_count(&http.get_text(&lei_count_url(iso2)?).await?)
}

pub async fn esef_count(http: &Http, iso2: &str) -> Result<u64> {
    parse_esef_count(&http.get_text(&esef_count_url(iso2)?).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_recorded_countries_and_drops_aggregates() {
        let countries = parse_countries(&crate::fixture("worldbank_countries.json")).unwrap();
        let codes: Vec<&str> = countries.iter().map(|c| c.iso2.as_str()).collect();
        assert_eq!(codes, ["JP", "SE", "US"], "the recording also holds the EU and the world total");
        let se = &countries[1];
        assert_eq!((se.iso3.as_str(), se.name.as_str(), se.capital.as_deref()), ("SWE", "Sweden", Some("Stockholm")));
        assert_eq!(se.region, "Europe & Central Asia");
        assert!(se.lat.unwrap() > 55.0 && se.lon.unwrap() > 10.0);
    }

    #[test]
    fn reads_recorded_gdp() {
        let gdp = parse_gdp(&crate::fixture("worldbank_gdp_all.json")).unwrap();
        let se = gdp.iter().find(|(c, _, _)| c == "SWE").unwrap();
        assert_eq!(se.1, 2025);
        assert!((se.2 - 668_998_664_082.081).abs() < 1.0);
    }

    #[test]
    fn reads_counts() {
        assert_eq!(parse_lei_count(&crate::fixture("gleif_country_count.json")).unwrap(), 121_615);
        assert_eq!(parse_esef_count(r#"{"data": [], "meta": {"count": 27817}}"#).unwrap(), 27_817);
        assert!(parse_lei_count("{}").is_err());
    }

    #[test]
    fn addresses_take_only_plain_codes() {
        assert!(lei_count_url("se").unwrap().contains("country%5D=SE&"));
        assert!(esef_count_url("S E").is_err());
    }
}
