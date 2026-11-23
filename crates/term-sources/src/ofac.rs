//! OFAC's list of Specially Designated Nationals (SDN), from the US
//! Treasury. No key.
//!
//! IMPORTANT: this is a screening aid for supply chain research and
//! nothing more. It reads the primary names only. Aliases live in a
//! separate file that is not read here, so "no match" does not mean that
//! a party is not sanctioned. Never use it as a compliance check.
//!
//! The list's free text remarks hold dates of birth and passport numbers
//! of private individuals. They are dropped on reading and never stored.

use crate::{format_err, Http, Result};
use term_core::SanctionEntry;

pub const URL: &str = "https://sanctionslistservice.ofac.treas.gov/api/PublicationPreview/exports/SDN.CSV";

/// OFAC writes this for an empty field.
const NULL: &str = "-0-";

fn field(record: &csv::StringRecord, index: usize) -> Option<String> {
    let value = record.get(index)?.trim();
    if value.is_empty() || value == NULL {
        None
    } else {
        Some(value.to_string())
    }
}

/// Columns: number, name, type, program, then eight more that are not kept.
pub fn parse(csv_text: &str) -> Result<Vec<SanctionEntry>> {
    let mut reader = csv::ReaderBuilder::new().has_headers(false).flexible(true).from_reader(csv_text.as_bytes());
    let mut entries = Vec::new();
    for row in reader.records() {
        let row = row.map_err(format_err)?;
        // The file ends with a lone end-of-file character on its own line.
        let Some(list_id) = row.get(0).and_then(|v| v.trim().parse::<u64>().ok()) else { continue };
        let Some(name) = field(&row, 1) else { continue };
        entries.push(SanctionEntry { list_id, name, kind: field(&row, 2), program: field(&row, 3) });
    }
    if entries.is_empty() {
        return Err(format_err("no entries found, the file does not look like SDN.CSV"));
    }
    Ok(entries)
}

pub async fn fetch(http: &Http) -> Result<Vec<SanctionEntry>> {
    parse(&http.get_text(URL).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_recorded_sample() {
        let entries = parse(&crate::fixture("ofac_sdn_sample.csv")).unwrap();
        assert_eq!(entries.len(), 5, "five rows, and the end-of-file line is skipped");
        assert_eq!(
            entries[0],
            SanctionEntry { list_id: 36, name: "AEROCARIBBEAN AIRLINES".into(), kind: None, program: Some("CUBA".into()) }
        );
        // A quoted name with a comma in it stays one field.
        assert_eq!(entries[1].name, "ANGLO-CARIBBEAN CO., LTD.");
        let vessel = entries.iter().find(|e| e.kind.as_deref() == Some("vessel")).unwrap();
        assert_eq!(vessel.name, "MAR AZUL");
    }

    #[test]
    fn something_else_is_not_accepted_as_a_list() {
        assert!(parse("<html>Service unavailable</html>").is_err());
        assert!(parse("").is_err());
    }
}
