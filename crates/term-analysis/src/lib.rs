//! Weighs everything the terminal knows about one company into a list of
//! findings: the chain, the review status, the sanctions list, the known
//! places and the stored events.
//!
//! Every finding says what it rests on. The analysis also lists what it
//! could NOT check, because a missing warning must never be mistaken for
//! an all clear. It describes exposure. It does not say what to buy or sell.

use serde::Serialize;
use term_core::time::unix_to_iso_minutes;
use term_core::{Cik, Direction, Event, Location, ReviewStatus, SanctionEntry, StoredRelation, Unit};
use term_store::{now_unix, Store, StoreError};

/// How far back events count.
pub const EVENT_WINDOW_DAYS: i64 = 30;
/// A single customer at or above this share of sales is flagged.
pub const WATCH_SHARE: f64 = 10.0;
pub const ALERT_SHARE: f64 = 20.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Something that limits what the analysis can say.
    Info,
    Watch,
    Alert,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Finding {
    pub level: Level,
    /// Short heading, for example `Koncentration`.
    pub area: &'static str,
    pub text: String,
    /// What the finding rests on.
    pub basis: String,
    /// The relation it concerns, when there is one.
    pub relation_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Analysis {
    pub company: String,
    pub suppliers: usize,
    pub customers: usize,
    pub pending: usize,
    /// Largest customer share of sales in percent, when one is disclosed.
    pub largest_customer_share: Option<f64>,
    /// Sum of all disclosed customer shares in percent.
    pub disclosed_customer_share: f64,
    pub places: usize,
    pub events_in_window: usize,
    /// Most serious first.
    pub findings: Vec<Finding>,
}

/// Everything the analysis reads. Built by [`analyze_company`], or by hand in tests.
pub struct Inputs<'a> {
    pub company: &'a str,
    /// Suppliers and customers. Rejected relations must already be removed.
    pub relations: &'a [StoredRelation],
    /// Sanction hits as (relation id, list entry).
    pub sanction_hits: &'a [(i64, SanctionEntry)],
    /// Whether a sanctions list is stored at all.
    pub sanctions_loaded: bool,
    /// Places of the company itself and of its counterparties.
    pub places: &'a [Location],
    pub events: &'a [Event],
    /// Unix seconds.
    pub now: i64,
}

/// Great circle distance in kilometres.
pub fn distance_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const EARTH_RADIUS_KM: f64 = 6371.0;
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = (lat2 - lat1).to_radians();
    let dl = (lon2 - lon1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_KM * a.sqrt().min(1.0).asin()
}

/// How close an event must be to a place to count, and how serious it is.
/// These radii are rules of thumb chosen for this prototype. They are not
/// taken from research and should be tuned against real cases.
fn reach(event: &Event) -> Option<(f64, Level)> {
    let red = event.alert.as_deref().is_some_and(|a| a.eq_ignore_ascii_case("red"));
    let orange = event.alert.as_deref().is_some_and(|a| a.eq_ignore_ascii_case("orange"));
    if event.kind == "earthquake" {
        return match event.severity {
            Some(m) if m >= 7.0 => Some((500.0, Level::Alert)),
            Some(m) if m >= 6.0 => Some((300.0, Level::Alert)),
            Some(m) if m >= 5.0 => Some((100.0, Level::Watch)),
            Some(_) => Some((50.0, Level::Watch)),
            None => None,
        };
    }
    if red {
        Some((300.0, Level::Alert))
    } else if orange {
        Some((300.0, Level::Watch))
    } else {
        None
    }
}

const SOLE_SOURCE_PHRASES: &[&str] =
    &["sole source", "sole-source", "single source", "single-source", "sole supplier", "only source", "only supplier"];

pub fn analyze(input: &Inputs<'_>) -> Analysis {
    let mut findings = Vec::new();
    let suppliers: Vec<&StoredRelation> =
        input.relations.iter().filter(|r| r.relation.direction == Direction::Supplier).collect();
    let customers: Vec<&StoredRelation> =
        input.relations.iter().filter(|r| r.relation.direction == Direction::Customer).collect();
    let pending = input.relations.iter().filter(|r| r.status == ReviewStatus::Pending).count();

    // ----- what the analysis stands on -----
    if input.relations.is_empty() {
        findings.push(Finding {
            level: Level::Info,
            area: "Underlag",
            text: "Ingen kedja är inläst för bolaget. Resten av analysen har inget att arbeta med.".into(),
            basis: "Relationstabellen".into(),
            relation_id: None,
        });
    } else if pending > 0 {
        findings.push(Finding {
            level: Level::Watch,
            area: "Underlag",
            text: format!(
                "{pending} av {} relationer är inte granskade av en människa. Fynden nedan kan vila på fel.",
                input.relations.len()
            ),
            basis: "Granskningsstatus".into(),
            relation_id: None,
        });
    }

    // ----- customer concentration -----
    let shares: Vec<(&StoredRelation, f64)> = customers
        .iter()
        .filter_map(|r| match &r.relation.amount {
            Some(a) if a.unit == Unit::Percent => Some((*r, a.value)),
            _ => None,
        })
        .collect();
    let largest = shares.iter().map(|(_, v)| *v).fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v))));
    let total: f64 = shares.iter().map(|(_, v)| *v).sum();
    for (relation, share) in &shares {
        let level = if *share >= ALERT_SHARE {
            Level::Alert
        } else if *share >= WATCH_SHARE {
            Level::Watch
        } else {
            continue;
        };
        findings.push(Finding {
            level,
            area: "Koncentration",
            text: format!("{} står för {share} % av försäljningen.", relation.relation.counterparty.name),
            basis: format!("Bolagets egen uppgift: \u{201d}{}\u{201d}", relation.relation.evidence.quote),
            relation_id: Some(relation.id),
        });
    }
    if shares.len() > 1 {
        findings.push(Finding {
            level: if total >= 50.0 { Level::Alert } else { Level::Info },
            area: "Koncentration",
            text: format!("{} namngivna kunder står tillsammans för {total} % av försäljningen.", shares.len()),
            basis: "Summan av redovisade andelar".into(),
            relation_id: None,
        });
    }

    // ----- customers by register dollars -----
    let dollars: Vec<(&StoredRelation, f64)> = customers
        .iter()
        .filter_map(|r| match &r.relation.amount {
            Some(a) if a.unit == Unit::Usd && a.value > 0.0 => Some((*r, a.value)),
            _ => None,
        })
        .collect();
    let dollar_total: f64 = dollars.iter().map(|(_, v)| *v).sum();
    if let Some((top, value)) = dollars.iter().max_by(|a, b| a.1.total_cmp(&b.1)) {
        if dollars.len() > 1 {
            let share = 100.0 * value / dollar_total;
            findings.push(Finding {
                level: if share >= 50.0 { Level::Watch } else { Level::Info },
                area: "Koncentration",
                text: format!(
                    "{} står för {share:.0} % av {:.1} miljoner USD i registrerade underkontrakt från {} kunder.",
                    top.relation.counterparty.name,
                    dollar_total / 1e6,
                    dollars.len()
                ),
                basis: "Registerdata. Täcker bara det som registret omfattar, inte bolagets hela försäljning.".into(),
                relation_id: Some(top.id),
            });
        }
    }

    // ----- single source suppliers -----
    for relation in &suppliers {
        let quote = relation.relation.evidence.quote.to_lowercase();
        if SOLE_SOURCE_PHRASES.iter().any(|p| quote.contains(p)) {
            findings.push(Finding {
                level: Level::Alert,
                area: "Enda källa",
                text: format!("{} beskrivs som enda källa.", relation.relation.counterparty.name),
                basis: format!("Bolagets egen uppgift: \u{201d}{}\u{201d}", relation.relation.evidence.quote),
                relation_id: Some(relation.id),
            });
        }
    }

    // ----- sanctions -----
    if !input.sanctions_loaded {
        findings.push(Finding {
            level: Level::Info,
            area: "Sanktioner",
            text: "Sanktionslistan är inte hämtad. Ingen motpart är jämförd.".into(),
            basis: "Kör: term sanctions fetch".into(),
            relation_id: None,
        });
    } else if !input.relations.is_empty() {
        for (relation_id, entry) in input.sanction_hits {
            let name = input
                .relations
                .iter()
                .find(|r| r.id == *relation_id)
                .map(|r| r.relation.counterparty.name.as_str())
                .unwrap_or("okänd motpart");
            findings.push(Finding {
                level: Level::Alert,
                area: "Sanktioner",
                text: format!("{name} har samma namn som en post på OFAC:s lista ({}).", entry.name),
                basis: format!(
                    "OFAC SDN nummer {}, program {}. Namnlikhet är inte identitet.",
                    entry.list_id,
                    entry.program.as_deref().unwrap_or("okänt")
                ),
                relation_id: Some(*relation_id),
            });
        }
        if input.sanction_hits.is_empty() {
            findings.push(Finding {
                level: Level::Info,
                area: "Sanktioner",
                text: format!("Ingen av {} motparter har ett huvudnamn på OFAC:s lista.", input.relations.len()),
                basis: "Bara huvudnamn jämförs, inte alias. Detta är ingen regelefterlevnadskontroll.".into(),
                relation_id: None,
            });
        }
    }

    // ----- events near known places -----
    let since = input.now - EVENT_WINDOW_DAYS * 86_400;
    let recent: Vec<&Event> = input.events.iter().filter(|e| e.time >= since && e.time <= input.now).collect();
    if input.places.is_empty() {
        findings.push(Finding {
            level: Level::Info,
            area: "Händelser",
            text: "Inga platser är kända för bolaget eller dess motparter. Händelser kan inte kopplas till kedjan.".into(),
            basis: "Lägg till med: term locations fetch-epa eller term locations add".into(),
            relation_id: None,
        });
    } else if input.events.is_empty() {
        findings.push(Finding {
            level: Level::Info,
            area: "Händelser",
            text: "Inga händelser är hämtade.".into(),
            basis: "Kör: term events fetch".into(),
            relation_id: None,
        });
    } else {
        let mut hits = 0;
        for event in &recent {
            let (Some(lat), Some(lon), Some((radius, level))) = (event.lat, event.lon, reach(event)) else { continue };
            // The closest place per owner is enough. One event should not flood the list.
            let mut nearest: Vec<(&Location, f64)> = Vec::new();
            for place in input.places {
                let km = distance_km(lat, lon, place.lat, place.lon);
                if km > radius {
                    continue;
                }
                match nearest.iter_mut().find(|(p, _)| p.owner == place.owner) {
                    Some(slot) if km < slot.1 => *slot = (place, km),
                    Some(_) => {}
                    None => nearest.push((place, km)),
                }
            }
            for (place, km) in nearest {
                hits += 1;
                findings.push(Finding {
                    level,
                    area: "Händelser",
                    text: format!("{} inträffade {km:.0} km från {} ({}).", event.title, place.label, place.owner),
                    basis: format!(
                        "{} {}, {}. Avståndsgräns {radius:.0} km är en tumregel.",
                        event.source,
                        event.external_id,
                        unix_to_iso_minutes(event.time)
                    ),
                    relation_id: None,
                });
            }
        }
        if hits == 0 {
            findings.push(Finding {
                level: Level::Info,
                area: "Händelser",
                text: format!(
                    "Ingen av {} händelser de senaste {EVENT_WINDOW_DAYS} dagarna ligger nära någon av {} kända platser.",
                    recent.len(),
                    input.places.len()
                ),
                basis: "Platslistan är sällan fullständig. Frånvaro av träff är inte frånvaro av risk.".into(),
                relation_id: None,
            });
        }
    }

    findings.sort_by(|a, b| b.level.cmp(&a.level).then(a.area.cmp(b.area)));
    Analysis {
        company: input.company.to_string(),
        suppliers: suppliers.len(),
        customers: customers.len(),
        pending,
        largest_customer_share: largest,
        disclosed_customer_share: total,
        places: input.places.len(),
        events_in_window: recent.len(),
        findings,
    }
}

/// Gather everything from the store and analyze one company.
pub fn analyze_company(store: &Store, cik: Cik) -> Result<Analysis, StoreError> {
    let view = store.chain(cik, None)?;
    let company = view.company.as_ref().map(|c| c.name.clone()).unwrap_or_else(|| format!("CIK {cik}"));
    let relations: Vec<StoredRelation> = view
        .suppliers
        .into_iter()
        .chain(view.customers)
        .filter(|r| r.status != ReviewStatus::Rejected)
        .collect();

    let sanctions_loaded = store.sanction_count()? > 0;
    let mut sanction_hits = Vec::new();
    let mut places = store.locations(Some(&company))?;
    for relation in &relations {
        for entry in store.sanction_matches(&relation.relation.counterparty.name)? {
            sanction_hits.push((relation.id, entry));
        }
        places.extend(store.locations(Some(&relation.relation.counterparty.name))?);
    }
    let events = store.events(5000)?;
    Ok(analyze(&Inputs {
        company: &company,
        relations: &relations,
        sanction_hits: &sanction_hits,
        sanctions_loaded,
        places: &places,
        events: &events,
        now: now_unix(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use term_core::{Amount, Company, Confidence, Counterparty, Evidence, Relation};

    const NOW: i64 = 1_795_500_000;

    fn relation(id: i64, name: &str, direction: Direction, pct: Option<f64>, quote: &str, status: ReviewStatus) -> StoredRelation {
        StoredRelation {
            id,
            relation: Relation {
                subject: Cik::new(42).unwrap(),
                counterparty: Counterparty { name: name.into(), cik: None },
                direction,
                amount: pct.map(|value| Amount { value, unit: Unit::Percent, basis: None }),
                period: None,
                evidence: Evidence { accession: None, url: None, quote: quote.into() },
                confidence: Confidence::Named,
                corroborated: false,
            },
            status,
            note: None,
            known_at: 0,
            reviewed_at: None,
        }
    }

    fn quake(id: &str, mag: f64, lat: f64, lon: f64, days_ago: i64) -> Event {
        Event {
            source: "usgs".into(),
            external_id: id.into(),
            kind: "earthquake".into(),
            title: format!("M {mag} test"),
            time: NOW - days_ago * 86_400,
            lat: Some(lat),
            lon: Some(lon),
            severity: Some(mag),
            severity_unit: None,
            alert: None,
            country: None,
            url: None,
        }
    }

    fn place(owner: &str, label: &str, lat: f64, lon: f64) -> Location {
        Location { owner: owner.into(), label: label.into(), lat, lon, source: "manual".into(), external_id: label.into() }
    }

    fn run(relations: &[StoredRelation], hits: &[(i64, SanctionEntry)], loaded: bool, places: &[Location], events: &[Event]) -> Analysis {
        analyze(&Inputs { company: "Test Co", relations, sanction_hits: hits, sanctions_loaded: loaded, places, events, now: NOW })
    }

    fn areas(a: &Analysis, level: Level) -> Vec<&'static str> {
        a.findings.iter().filter(|f| f.level == level).map(|f| f.area).collect()
    }

    #[test]
    fn distance_matches_known_cities() {
        // Stockholm to Gothenburg is about 398 km as the crow flies.
        let km = distance_km(59.3293, 18.0686, 57.7089, 11.9746);
        assert!((km - 398.0).abs() < 3.0, "{km}");
        assert!(distance_km(10.0, 20.0, 10.0, 20.0) < 1e-6);
        // Across the date line, not around the world.
        assert!(distance_km(0.0, 179.5, 0.0, -179.5) < 120.0);
    }

    #[test]
    fn empty_company_says_so_and_claims_nothing() {
        let a = run(&[], &[], false, &[], &[]);
        assert_eq!(areas(&a, Level::Alert), Vec::<&str>::new());
        assert!(a.findings.iter().any(|f| f.area == "Underlag" && f.text.contains("Ingen kedja")));
        assert!(a.findings.iter().any(|f| f.area == "Sanktioner" && f.text.contains("inte hämtad")));
        assert!(a.findings.iter().any(|f| f.area == "Händelser" && f.text.contains("Inga platser")));
    }

    #[test]
    fn concentration_levels_follow_the_thresholds() {
        let relations = [
            relation(1, "Big Buyer", Direction::Customer, Some(21.0), "q1", ReviewStatus::Approved),
            relation(2, "Mid Buyer", Direction::Customer, Some(12.5), "q2", ReviewStatus::Approved),
            relation(3, "Small Buyer", Direction::Customer, Some(4.0), "q3", ReviewStatus::Approved),
        ];
        let a = run(&relations, &[], true, &[], &[]);
        assert_eq!(a.largest_customer_share, Some(21.0));
        assert_eq!(a.disclosed_customer_share, 37.5);
        let conc: Vec<_> = a.findings.iter().filter(|f| f.area == "Koncentration").collect();
        assert_eq!(conc.iter().filter(|f| f.level == Level::Alert).count(), 1);
        assert_eq!(conc.iter().filter(|f| f.level == Level::Watch).count(), 1);
        assert!(conc.iter().any(|f| f.text.contains("37.5 %")));
        assert!(!conc.iter().any(|f| f.text.contains("Small Buyer")), "4 percent is below the threshold");
        assert_eq!(conc.iter().find(|f| f.level == Level::Alert).unwrap().relation_id, Some(1));
    }

    #[test]
    fn register_dollars_give_a_share_with_a_caveat() {
        let usd = |id, name: &str, value| {
            let mut r = relation(id, name, Direction::Customer, None, "q", ReviewStatus::Approved);
            r.relation.amount = Some(Amount { value, unit: Unit::Usd, basis: None });
            r
        };
        let a = run(&[usd(1, "Prime A", 3_000_000.0), usd(2, "Prime B", 1_000_000.0)], &[], true, &[], &[]);
        let f = a.findings.iter().find(|f| f.text.contains("underkontrakt")).unwrap();
        assert_eq!((f.level, f.relation_id), (Level::Watch, Some(1)));
        assert!(f.text.contains("Prime A står för 75 % av 4.0 miljoner USD"), "{}", f.text);
        assert!(f.basis.contains("inte bolagets hela försäljning"));
        // A single register customer says nothing about concentration.
        let one = run(&[usd(1, "Prime A", 3_000_000.0)], &[], true, &[], &[]);
        assert!(!one.findings.iter().any(|f| f.text.contains("underkontrakt")));
    }

    #[test]
    fn sole_source_is_read_from_the_quote() {
        let relations = [
            relation(1, "Chip Maker", Direction::Supplier, None, "Chip Maker is our sole source", ReviewStatus::Approved),
            relation(2, "Housing Maker", Direction::Supplier, None, "supplied by Housing Maker", ReviewStatus::Approved),
        ];
        let a = run(&relations, &[], true, &[], &[]);
        let sole: Vec<_> = a.findings.iter().filter(|f| f.area == "Enda källa").collect();
        assert_eq!(sole.len(), 1);
        assert_eq!((sole[0].level, sole[0].relation_id), (Level::Alert, Some(1)));
    }

    #[test]
    fn unreviewed_relations_are_called_out() {
        let relations = [
            relation(1, "A Co", Direction::Customer, None, "q", ReviewStatus::Pending),
            relation(2, "B Co", Direction::Supplier, None, "q", ReviewStatus::Approved),
        ];
        let a = run(&relations, &[], true, &[], &[]);
        assert_eq!(a.pending, 1);
        assert!(a.findings.iter().any(|f| f.area == "Underlag" && f.level == Level::Watch && f.text.contains("1 av 2")));
    }

    #[test]
    fn sanction_hit_is_an_alert_and_no_hit_is_only_information() {
        let relations = [relation(7, "Banco Nacional de Cuba", Direction::Customer, None, "q", ReviewStatus::Approved)];
        let entry = SanctionEntry { list_id: 306, name: "BANCO NACIONAL DE CUBA".into(), kind: None, program: Some("CUBA".into()) };
        let hit = run(&relations, &[(7, entry)], true, &[], &[]);
        let f = hit.findings.iter().find(|f| f.area == "Sanktioner").unwrap();
        assert_eq!((f.level, f.relation_id), (Level::Alert, Some(7)));
        assert!(f.basis.contains("Namnlikhet är inte identitet"));

        let clear = run(&relations, &[], true, &[], &[]);
        let f = clear.findings.iter().find(|f| f.area == "Sanktioner").unwrap();
        assert_eq!(f.level, Level::Info);
        assert!(f.basis.contains("inte alias"));
    }

    #[test]
    fn events_count_only_when_near_recent_and_strong_enough() {
        let relations = [relation(1, "Chip Maker", Direction::Supplier, None, "q", ReviewStatus::Approved)];
        let places = [place("Chip Maker", "Fab 1", 35.0, 139.0), place("Chip Maker", "Fab 2", 35.5, 139.0)];
        let events = [
            quake("near-strong", 6.4, 35.2, 139.1, 2),  // about 24 km from Fab 1: alert
            quake("near-weak", 4.6, 35.0, 140.5, 2),    // about 137 km away, limit 50: nothing
            quake("far-strong", 7.5, -20.0, -70.0, 2),  // other side of the world: nothing
            quake("old-strong", 6.8, 35.1, 139.0, 45),  // too long ago: nothing
        ];
        let a = run(&relations, &[], true, &places, &events);
        let found: Vec<_> = a.findings.iter().filter(|f| f.area == "Händelser").collect();
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(found[0].level, Level::Alert);
        assert!(found[0].text.contains("M 6.4") && found[0].text.contains("Fab 1"), "the nearest place is named");
        assert!(found[0].basis.contains("near-strong"));
        assert_eq!(a.events_in_window, 3);
    }

    #[test]
    fn no_nearby_event_is_reported_as_no_proof_of_safety() {
        let places = [place("Test Co", "HQ", 59.3, 18.0)];
        let a = run(&[], &[], true, &places, &[quake("far", 6.0, -20.0, -70.0, 1)]);
        let f = a.findings.iter().find(|f| f.area == "Händelser").unwrap();
        assert_eq!(f.level, Level::Info);
        assert!(f.basis.contains("inte frånvaro av risk"));
    }

    #[test]
    fn red_alert_cyclone_reaches_three_hundred_km() {
        let mut cyclone = quake("tc", 150.0, 15.8, 80.2, 3);
        cyclone.kind = "cyclone".into();
        cyclone.alert = Some("Red".into());
        cyclone.title = "Tropical Cyclone TEST".into();
        let near = [place("Port Co", "Terminal", 16.5, 81.5)]; // roughly 160 km away
        let far = [place("Port Co", "Terminal", 22.0, 88.0)];
        assert!(run(&[], &[], true, &near, std::slice::from_ref(&cyclone)).findings.iter().any(|f| f.level == Level::Alert));
        assert!(!run(&[], &[], true, &far, &[cyclone]).findings.iter().any(|f| f.level == Level::Alert));
    }

    #[test]
    fn most_serious_findings_come_first() {
        let relations = [
            relation(1, "Big Buyer", Direction::Customer, Some(30.0), "q", ReviewStatus::Pending),
            relation(2, "Chip Maker", Direction::Supplier, None, "our sole source", ReviewStatus::Approved),
        ];
        let a = run(&relations, &[], false, &[], &[]);
        let levels: Vec<Level> = a.findings.iter().map(|f| f.level).collect();
        let mut sorted = levels.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(levels, sorted);
        assert_eq!(a.findings[0].level, Level::Alert);
    }

    #[test]
    fn gathers_from_a_real_store() {
        let store = Store::open_in_memory().unwrap();
        let cik = Cik::new(42).unwrap();
        store.upsert_company(&Company { cik, name: "Nordvik Test Components Inc.".into(), ticker: None }).unwrap();
        let big = relation(0, "Haldenfjord Motors Corp", Direction::Customer, Some(21.0), "q", ReviewStatus::Pending).relation;
        let gone = relation(0, "Rejected Co", Direction::Customer, Some(40.0), "q", ReviewStatus::Pending).relation;
        store.upsert_relation(&big).unwrap();
        let rejected = store.upsert_relation(&gone).unwrap().id();
        store.set_review(rejected, ReviewStatus::Rejected, None).unwrap();
        store.upsert_locations(&[place("Nordvik Test Components", "Plant", 35.0, 139.0)]).unwrap();
        store.upsert_events(&[quake("q1", 6.5, 35.1, 139.0, 0)]).unwrap();
        store
            .replace_sanctions("ofac", &[SanctionEntry { list_id: 1, name: "Unrelated Name".into(), kind: None, program: None }])
            .unwrap();

        let a = analyze_company(&store, cik).unwrap();
        assert_eq!(a.company, "Nordvik Test Components Inc.");
        assert_eq!((a.customers, a.suppliers, a.places), (1, 0, 1), "the rejected relation is left out");
        assert_eq!(a.largest_customer_share, Some(21.0));
        // The stored event is dated in the fixture's future relative to the wall clock,
        // or in its past. Either way the sanctions and concentration findings must be there.
        assert!(a.findings.iter().any(|f| f.area == "Koncentration" && f.level == Level::Alert));
        assert!(a.findings.iter().any(|f| f.area == "Sanktioner" && f.level == Level::Info));
    }
}
