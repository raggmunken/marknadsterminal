//! Part 3 of the plan: the ground the signal engine stands on.
//!
//! What is here: the signal card as a type, the tamper evident signal log,
//! the four gates as a state machine, and the arithmetic the gates need.
//!
//! What is NOT here: any rule that produces a signal. The three signal
//! types in the plan need price history, estimates and event data that
//! the project does not have yet. Nothing in this crate suggests a trade.

pub mod gates;
pub mod log;
pub mod stats;

use serde::{Deserialize, Serialize};
use term_core::Evidence;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Buy,
    Sell,
}

/// A signal is shown as a paper signal until its rule has passed gate 4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SignalStatus {
    Paper,
    Approved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleRef {
    pub id: String,
    pub version: u32,
}

/// How the rule has done so far. Shown on the card as "how certain".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackRecord {
    pub signals: u32,
    pub hits: u32,
    pub mean_net_return: f64,
}

/// The signal card: what, where, when, how certain and why.
///
/// There is no amount and no position size on the card. The card proposes.
/// The person decides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Signal {
    /// What: instrument identifier (FIGI or ticker) and side.
    pub instrument: String,
    pub side: Side,
    /// Where: exchange and trading currency.
    pub venue: String,
    pub currency: String,
    /// When: ISO dates for the entry window, then holding period and exit.
    pub entry_from: String,
    pub entry_to: String,
    pub horizon_days: u32,
    pub exit_rule: String,
    /// How certain. `None` until the rule has a history.
    pub track_record: Option<TrackRecord>,
    /// Why: the evidence that triggered the rule.
    pub trigger: Vec<Evidence>,
    pub rule: RuleRef,
    pub status: SignalStatus,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SignalError {
    #[error("signal is missing {0}")]
    Missing(&'static str),
    #[error("{0} is not an ISO date (YYYY-MM-DD)")]
    BadDate(String),
    #[error("entry window ends before it starts")]
    WindowReversed,
}

fn is_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    if !(digits(0..4) && digits(5..7) && digits(8..10)) {
        return false;
    }
    let month: u32 = s[5..7].parse().unwrap_or(0);
    let day: u32 = s[8..10].parse().unwrap_or(0);
    (1..=12).contains(&month) && (1..=31).contains(&day)
}

impl Signal {
    /// A card with a hole in it is not shown and not logged.
    pub fn validate(&self) -> Result<(), SignalError> {
        if self.instrument.trim().is_empty() {
            return Err(SignalError::Missing("instrument"));
        }
        if self.venue.trim().is_empty() {
            return Err(SignalError::Missing("venue"));
        }
        if self.currency.trim().is_empty() {
            return Err(SignalError::Missing("currency"));
        }
        if self.exit_rule.trim().is_empty() {
            return Err(SignalError::Missing("exit rule"));
        }
        if self.horizon_days == 0 {
            return Err(SignalError::Missing("horizon"));
        }
        if self.trigger.is_empty() {
            return Err(SignalError::Missing("trigger evidence"));
        }
        for date in [&self.entry_from, &self.entry_to] {
            if !is_iso_date(date) {
                return Err(SignalError::BadDate(date.clone()));
            }
        }
        // ISO dates sort as text.
        if self.entry_to < self.entry_from {
            return Err(SignalError::WindowReversed);
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn example_signal() -> Signal {
    Signal {
        instrument: "TEST".into(),
        side: Side::Buy,
        venue: "XTST".into(),
        currency: "USD".into(),
        entry_from: "2027-08-09".into(),
        entry_to: "2027-08-13".into(),
        horizon_days: 60,
        exit_rule: "close after 60 trading days".into(),
        track_record: None,
        trigger: vec![Evidence { accession: None, url: None, quote: "made up trigger for tests".into() }],
        rule: RuleRef { id: "kundchock".into(), version: 1 },
        status: SignalStatus::Paper,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_card_is_valid() {
        assert_eq!(example_signal().validate(), Ok(()));
    }

    #[test]
    fn card_needs_every_field() {
        let mut s = example_signal();
        s.trigger.clear();
        assert_eq!(s.validate(), Err(SignalError::Missing("trigger evidence")));

        let mut s = example_signal();
        s.exit_rule = "  ".into();
        assert_eq!(s.validate(), Err(SignalError::Missing("exit rule")));

        let mut s = example_signal();
        s.horizon_days = 0;
        assert_eq!(s.validate(), Err(SignalError::Missing("horizon")));
    }

    #[test]
    fn dates_are_checked() {
        let mut s = example_signal();
        s.entry_to = "2027-08-01".into();
        assert_eq!(s.validate(), Err(SignalError::WindowReversed));

        for bad in ["2027-13-01", "2027-8-9", "9 augusti", "2027-08-32", ""] {
            let mut s = example_signal();
            s.entry_from = bad.into();
            assert_eq!(s.validate(), Err(SignalError::BadDate(bad.into())), "{bad:?}");
        }
    }
}
