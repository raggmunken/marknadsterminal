//! The whole chain on the synthetic fixture, scored against its answer key.
//!
//! The fixture is invented. Passing here shows that the plumbing works.
//! It says nothing about quality on real filings.

use std::path::PathBuf;
use term_core::{Cik, Confidence, Direction};
use term_extract::baseline::BaselineExtractor;
use term_extract::chain::{run, SourceRef};
use term_extract::eval::{evaluate, FacitRow};
use term_parse::{html_to_text, DEFAULT_MIN_SCORE};

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[tokio::test]
async fn baseline_on_synthetic_filing() {
    let text = html_to_text(&fixture("syntetisk_10k.html"));
    let facit: Vec<FacitRow> = serde_json::from_str(&fixture("syntetisk_10k.facit.json")).unwrap();

    // The script block and the comment must not leak into the text.
    assert!(!text.contains("Fake Analytics"));
    assert!(!text.contains("invented for testing"));

    let out = run(Cik::new(42).unwrap(), &text, &SourceRef::default(), &BaselineExtractor, DEFAULT_MIN_SCORE)
        .await
        .unwrap();
    let scores = evaluate(&out.accepted, &facit);
    eprintln!("{scores:#?}");

    // Everything the baseline reports is right.
    assert_eq!(scores.wrong, Vec::<String>::new());
    assert_eq!(scores.precision, 1.0);

    // It finds four of five. The fifth is phrased as "roughly one fifth",
    // which no fixed pattern covers. That miss is the reason a language
    // model is in the plan, and this assertion keeps the gap visible.
    assert_eq!(scores.correct, 4);
    assert_eq!(scores.missed, vec!["tallgrass distribution (customer)"]);

    // The unnamed "Customer C" must not become a relation.
    assert!(out.accepted.iter().all(|r| !r.counterparty.name.starts_with("Customer")));

    let halden = out.accepted.iter().find(|r| r.counterparty.name.starts_with("Haldenfjord")).unwrap();
    assert_eq!(halden.direction, Direction::Customer);
    assert_eq!(halden.confidence, Confidence::Disclosed);
    assert_eq!(halden.amount.as_ref().unwrap().value, 21.0);

    let kestrel = out.accepted.iter().find(|r| r.counterparty.name.starts_with("Kestrelmark")).unwrap();
    assert_eq!(kestrel.direction, Direction::Supplier);
    assert_eq!(kestrel.confidence, Confidence::Named);
}
