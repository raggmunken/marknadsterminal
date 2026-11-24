//! Chain engine steps 2 to 4 wired together: pick passages, extract,
//! verify quotes. Steps 1 (document choice) and 5 to 8 are not built yet.

use term_core::names::normalize_name;
use crate::quote::verify_quote;
use crate::{ExtractError, Extractor, RawRelation};
use futures_util::StreamExt;
use serde::Serialize;
use std::collections::BTreeMap;
use term_core::{Cik, Confidence, Counterparty, Evidence, Period, Relation};
use term_parse::{select_passages, select_passages_wide, Passage};

/// Which document the text came from. Copied into every piece of evidence.
#[derive(Debug, Clone, Default)]
pub struct SourceRef {
    pub accession: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Rejected {
    pub raw: RawRelation,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChainOutput {
    pub extractor: String,
    pub passages: Vec<Passage>,
    pub accepted: Vec<Relation>,
    pub rejected: Vec<Rejected>,
}

/// Run steps 2 to 4 on the plain text of one document.
pub async fn run<E: Extractor>(
    subject: Cik,
    text: &str,
    source: &SourceRef,
    extractor: &E,
    min_score: u32,
) -> Result<ChainOutput, ExtractError> {
    // A model reads free text, so it gets the wider selection, capped so that one
    // long report does not mean hundreds of calls. The cap is `TERM_MAX_PASSAGES`.
    let passages = if extractor.reads_free_text() {
        let max = std::env::var("TERM_MAX_PASSAGES").ok().and_then(|v| v.parse().ok()).unwrap_or(60);
        select_passages_wide(text, 3, max)
    } else {
        select_passages(text, min_score)
    };
    let mut accepted: BTreeMap<(String, bool), Relation> = BTreeMap::new();
    let mut rejected = Vec::new();

    // Passages go to the extractor several at a time. The results are read in
    // passage order, so the outcome does not depend on which call finishes first.
    let concurrency = std::env::var("TERM_LLM_CONCURRENCY")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(8)
        .max(1);
    let answers: Vec<Result<Vec<RawRelation>, ExtractError>> = futures_util::stream::iter(passages.iter())
        .map(|p| extractor.extract(&p.text))
        .buffered(concurrency)
        .collect()
        .await;

    for (passage, answer) in passages.iter().zip(answers) {
        for raw in answer? {
            // The quote must be in the passage the extractor was shown,
            // not merely somewhere in the document.
            if let Err(e) = verify_quote(&passage.text, &raw.quote) {
                rejected.push(Rejected { raw, reason: e.to_string() });
                continue;
            }
            if raw.counterparty.trim().is_empty() {
                rejected.push(Rejected { raw, reason: "empty counterparty".into() });
                continue;
            }
            let key = (
                normalize_name(&raw.counterparty),
                matches!(raw.direction, term_core::Direction::Customer),
            );
            let relation = to_relation(subject, source, raw);
            // Keep one relation per counterparty and direction. A relation
            // with an amount beats one without.
            match accepted.get(&key) {
                Some(existing) if existing.amount.is_some() || relation.amount.is_none() => {}
                _ => {
                    accepted.insert(key, relation);
                }
            }
        }
    }

    Ok(ChainOutput {
        extractor: extractor.name().to_string(),
        passages,
        accepted: accepted.into_values().collect(),
        rejected,
    })
}

fn to_relation(subject: Cik, source: &SourceRef, raw: RawRelation) -> Relation {
    let confidence = if raw.amount.is_some() { Confidence::Disclosed } else { Confidence::Named };
    Relation {
        subject,
        counterparty: Counterparty { name: raw.counterparty.trim().to_string(), cik: None },
        direction: raw.direction,
        amount: raw.amount,
        period: raw.fiscal_year.map(|fiscal_year| Period { fiscal_year }),
        evidence: Evidence {
            accession: source.accession.clone(),
            url: source.url.clone(),
            quote: raw.quote,
        },
        confidence,
        corroborated: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::baseline::BaselineExtractor;
    use term_core::Direction;
    use term_parse::DEFAULT_MIN_SCORE;

    /// Stands in for a model that makes things up.
    struct Liar;

    impl Extractor for Liar {
        fn name(&self) -> &str {
            "liar"
        }
        async fn extract(&self, _passage: &str) -> Result<Vec<RawRelation>, ExtractError> {
            Ok(vec![
                RawRelation {
                    counterparty: "Invented Holdings".into(),
                    direction: Direction::Customer,
                    amount: None,
                    fiscal_year: None,
                    quote: "Invented Holdings accounted for 40% of our net sales".into(),
                },
                RawRelation {
                    counterparty: "Example Motors Corp.".into(),
                    direction: Direction::Customer,
                    amount: None,
                    fiscal_year: None,
                    quote: "Example Motors Corp. accounted for 21% of our net sales".into(),
                },
            ])
        }
    }

    const TEXT: &str = "Risk factors are described elsewhere in this report and should be read with care.\n\
        In fiscal 2025, Example Motors Corp. accounted for 21% of our net sales. Our housings are supplied by Polar Castings Ltd.";

    fn subject() -> Cik {
        Cik::new(42).unwrap()
    }

    #[tokio::test]
    async fn baseline_runs_end_to_end() {
        let source = SourceRef { accession: Some("0000000042-25-000001".into()), url: None };
        let out = run(subject(), TEXT, &source, &BaselineExtractor, DEFAULT_MIN_SCORE).await.unwrap();
        assert_eq!(out.passages.len(), 1);
        assert!(out.rejected.is_empty());
        assert_eq!(out.accepted.len(), 2);
        let customer = out.accepted.iter().find(|r| r.direction == Direction::Customer).unwrap();
        assert_eq!(customer.confidence, Confidence::Disclosed);
        assert_eq!(customer.period, Some(Period { fiscal_year: 2025 }));
        assert_eq!(customer.evidence.accession.as_deref(), Some("0000000042-25-000001"));
        let supplier = out.accepted.iter().find(|r| r.direction == Direction::Supplier).unwrap();
        assert_eq!(supplier.confidence, Confidence::Named);
    }

    #[tokio::test]
    async fn invented_quotes_are_rejected() {
        let out = run(subject(), TEXT, &SourceRef::default(), &Liar, DEFAULT_MIN_SCORE).await.unwrap();
        assert_eq!(out.accepted.len(), 1);
        assert_eq!(out.accepted[0].counterparty.name, "Example Motors Corp.");
        assert_eq!(out.rejected.len(), 1);
        assert_eq!(out.rejected[0].raw.counterparty, "Invented Holdings");
        assert!(out.rejected[0].reason.contains("does not occur"));
    }

    #[tokio::test]
    async fn duplicate_mentions_collapse_and_amount_wins() {
        let text = "Parts are supplied by Polar Castings Ltd under a long term customer agreement.\n\
                    Example Motors Corp. is a significant customer. Example Motors Corp. accounted for 21% of our net sales.\n\
                    Example Motors Corp accounted for 21% of net sales and remains our largest customer.";
        let out = run(subject(), text, &SourceRef::default(), &BaselineExtractor, DEFAULT_MIN_SCORE).await.unwrap();
        let customers: Vec<_> = out.accepted.iter().filter(|r| r.direction == Direction::Customer).collect();
        assert_eq!(customers.len(), 1);
        assert!(customers[0].amount.is_some());
    }
}
