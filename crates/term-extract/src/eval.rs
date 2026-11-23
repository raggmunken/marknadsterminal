//! Scoring against a hand made answer key ("facit").
//!
//! The answer key is written by people. No extractor may edit it.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use term_core::{Direction, Relation};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FacitRow {
    pub counterparty: String,
    pub direction: Direction,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Scores {
    pub predicted: usize,
    pub expected: usize,
    pub correct: usize,
    /// Share of predicted relations that are in the answer key. 1.0 when nothing is predicted.
    pub precision: f64,
    /// Share of the answer key that was found. 1.0 when the key is empty.
    pub recall: f64,
    pub missed: Vec<String>,
    pub wrong: Vec<String>,
}

pub use term_core::names::normalize_name;

fn key(name: &str, direction: Direction) -> (String, &'static str) {
    let d = match direction {
        Direction::Customer => "customer",
        Direction::Supplier => "supplier",
    };
    (normalize_name(name), d)
}

pub fn evaluate(predicted: &[Relation], facit: &[FacitRow]) -> Scores {
    let predicted_keys: BTreeSet<_> =
        predicted.iter().map(|r| key(&r.counterparty.name, r.direction)).collect();
    let expected_keys: BTreeSet<_> =
        facit.iter().map(|r| key(&r.counterparty, r.direction)).collect();

    let correct = predicted_keys.intersection(&expected_keys).count();
    let label = |k: &(String, &str)| format!("{} ({})", k.0, k.1);
    let ratio = |part: usize, whole: usize| if whole == 0 { 1.0 } else { part as f64 / whole as f64 };

    Scores {
        predicted: predicted_keys.len(),
        expected: expected_keys.len(),
        correct,
        precision: ratio(correct, predicted_keys.len()),
        recall: ratio(correct, expected_keys.len()),
        missed: expected_keys.difference(&predicted_keys).map(label).collect(),
        wrong: predicted_keys.difference(&expected_keys).map(label).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use term_core::{Cik, Confidence, Counterparty, Evidence};

    fn rel(name: &str, direction: Direction) -> Relation {
        Relation {
            subject: Cik::new(1).unwrap(),
            counterparty: Counterparty { name: name.into(), cik: None },
            direction,
            amount: None,
            period: None,
            evidence: Evidence { accession: None, url: None, quote: String::new() },
            confidence: Confidence::Named,
            corroborated: false,
        }
    }

    fn row(name: &str, direction: Direction) -> FacitRow {
        FacitRow { counterparty: name.into(), direction }
    }

    #[test]
    fn counts_hits_misses_and_wrong_answers() {
        let predicted = [rel("Alpha Inc.", Direction::Customer), rel("Gamma Ltd", Direction::Supplier)];
        let facit = [
            row("Alpha Inc", Direction::Customer),
            row("Beta Corp", Direction::Customer),
            row("Gamma Ltd", Direction::Customer),
        ];
        let s = evaluate(&predicted, &facit);
        assert_eq!((s.predicted, s.expected, s.correct), (2, 3, 1));
        assert_eq!(s.precision, 0.5);
        assert!((s.recall - 1.0 / 3.0).abs() < 1e-9);
        assert_eq!(s.wrong, vec!["gamma (supplier)"]);
        assert_eq!(s.missed.len(), 2);
    }

    #[test]
    fn empty_inputs_do_not_divide_by_zero() {
        let s = evaluate(&[], &[]);
        assert_eq!((s.precision, s.recall), (1.0, 1.0));
        let s = evaluate(&[], &[row("A B", Direction::Customer)]);
        assert_eq!((s.precision, s.recall), (1.0, 0.0));
    }
}
